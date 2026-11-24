#!/usr/bin/env python3
"""Online build tooling only. The Rust launcher has no download client.

resolve: pin upstream metadata and hashes; prepare: download verified content;
pack: append a compressed payload and its checksum footer to an executable.
"""
import argparse
import concurrent.futures
import gzip
import hashlib
import http.client
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import struct
import sys
import tarfile
import threading
import time
import urllib.parse
import urllib.request
import zipfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / ".cache" / "downloads"
MAGIC = b"TRANSCRAFT1END!!"
PLATFORMS = ("linux", "windows")
AGENT = "Transcraft/0.1.0 (personal offline bundle builder)"
CONNECTIONS = threading.local()
PARTS = concurrent.futures.ThreadPoolExecutor(max_workers=12)


def fetch(url):
    if not url.startswith("https://"):
        raise ValueError(f"HTTPS required: {url}")
    for attempt in range(4):
        try:
            # Thousands of small assets share one host. Reuse TLS connections per
            # worker instead of paying for a fresh handshake for every object.
            parsed = urllib.parse.urlsplit(url)
            if parsed.netloc == "resources.download.minecraft.net" and not urllib.request.getproxies().get("https"):
                connection = getattr(CONNECTIONS, "assets", None)
                if connection is None:
                    connection = http.client.HTTPSConnection(parsed.netloc, timeout=90)
                    CONNECTIONS.assets = connection
                connection.request("GET", parsed.path, headers={"User-Agent": AGENT})
                response = connection.getresponse()
                data = response.read()
                if response.status != 200:
                    raise OSError(f"Asset server returned HTTP {response.status}")
                return data
            with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": AGENT}), timeout=90) as response:
                return response.read()
        except Exception:
            connection = getattr(CONNECTIONS, "assets", None)
            if connection:
                connection.close()
                CONNECTIONS.assets = None
            if attempt == 3:
                raise
            time.sleep(attempt + 1)


def remote_json(url):
    return json.loads(fetch(url))


def fetch_binary(url):
    """Bound retries to a 1 MiB chunk on large, range-capable downloads."""
    if not url.startswith("https://"):
        raise ValueError("HTTPS required")
    chunk_size = 1024 * 1024
    def request(start, end, total=None):
        for attempt in range(4):
            try:
                req = urllib.request.Request(url, headers={"User-Agent": AGENT, "Range": f"bytes={start}-{end}"})
                with urllib.request.urlopen(req, timeout=90) as response:
                    if response.status == 200 and start == 0:
                        return response.read(), None
                    header = response.headers.get("Content-Range", "")
                    match = re.fullmatch(r"bytes (\d+)-(\d+)/(\d+)", header)
                    if response.status != 206 or not match:
                        raise ValueError("Invalid byte-range response")
                    a, b, size = map(int, match.groups())
                    if a != start or b != min(end, size - 1) or (total is not None and total != size):
                        raise ValueError("Mismatched byte range")
                    data = response.read()
                    if len(data) != b - a + 1:
                        raise ValueError("Truncated byte range")
                    return data, size
            except Exception:
                if attempt == 3:
                    raise
                time.sleep(attempt + 1)
    first, total = request(0, chunk_size - 1)
    if total is None or len(first) == total:
        return first
    jobs = [PARTS.submit(request, start, min(start + chunk_size, total) - 1, total)
            for start in range(len(first), total, chunk_size)]
    return first + b"".join(job.result()[0] for job in jobs)


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def safe_path(name):
    p = PurePosixPath(name)
    if not name or p.is_absolute() or ".." in p.parts or "\\" in name or ":" in name:
        raise ValueError(f"Unsafe archive path: {name}")
    return p


def allowed(rules, platform):
    if rules is None:
        return True
    result = False
    for rule in rules:
        os_rule = rule.get("os", {})
        if os_rule.get("name", platform) != platform:
            continue
        if os_rule.get("arch", "x86_64") not in ("x86_64", "amd64"):
            continue
        if "version" in os_rule or "versionRange" in os_rule:
            raise ValueError("OS-version rules require explicit review")
        if any(value != (key == "has_custom_resolution") for key, value in rule.get("features", {}).items()):
            continue
        result = rule["action"] == "allow"
    return result


def arguments(items, platform):
    result = []
    for item in items:
        if isinstance(item, str):
            result.append(item)
        elif allowed(item.get("rules"), platform):
            value = item["value"]
            result.extend([value] if isinstance(value, str) else value)
    return result


def maven_path(name):
    group, artifact, version, *classifier = name.split(":")
    suffix = "-" + classifier[0] if classifier else ""
    return f"{group.replace('.', '/')}/{artifact}/{version}/{artifact}-{version}{suffix}.jar"


def descriptor(data, path):
    algo = "sha256" if "sha256" in data else "sha1"
    return {"path": str(safe_path(path)), "url": data["url"], "algorithm": algo, "hash": data[algo]}


def resolve(_args):
    version = "26.3"
    listing = remote_json("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
    entry = next(v for v in listing["versions"] if v["id"] == version)
    raw = fetch(entry["url"])
    if hashlib.sha1(raw).hexdigest() != entry["sha1"]:
        raise ValueError("Minecraft metadata checksum mismatch")
    mc = json.loads(raw)
    loader = next(v["loader"]["version"] for v in remote_json(f"https://meta.fabricmc.net/v2/versions/loader/{version}") if v["loader"]["stable"])
    fabric = remote_json(f"https://meta.fabricmc.net/v2/versions/loader/{version}/{loader}/profile/json")
    common = [descriptor(mc["downloads"]["client"], "versions/26.3/client.jar")]
    ai = mc["assetIndex"]
    common.append(descriptor(ai, f"assets/indexes/{ai['id']}.json"))
    index_raw = fetch(ai["url"])
    if hashlib.sha1(index_raw).hexdigest() != ai["sha1"]:
        raise ValueError("Asset index checksum mismatch")
    for h in sorted({o["hash"] for o in json.loads(index_raw)["objects"].values()}):
        common.append({"path": f"assets/objects/{h[:2]}/{h}", "url": f"https://resources.download.minecraft.net/{h[:2]}/{h}", "algorithm": "sha1", "hash": h})
    mods = []
    for project, supplied in [("sodium", "sodium-fabric-0.9.3+mc26.3.jar"), ("lithium", "lithium-fabric-0.26.4+mc26.3.jar"), ("modmenu", "modmenu-21.0.1.jar"), ("fabric-api", None)]:
        if supplied:
            local = ROOT / supplied
            if not local.exists():
                local = ROOT / "mods" / supplied
            h = hashlib.sha512(local.read_bytes()).hexdigest()
            v = remote_json(f"https://api.modrinth.com/v2/version_file/{h}?algorithm=sha512")
            file = next(f for f in v["files"] if f["hashes"]["sha512"] == h)
        else:
            query = urllib.parse.urlencode({"game_versions": json.dumps([version]), "loaders": '["fabric"]'})
            versions = remote_json(f"https://api.modrinth.com/v2/project/{project}/version?{query}")
            v = next((v for v in versions if v["version_type"] == "release"), versions[0])
            file = next(f for f in v["files"] if f["primary"])
        mods.append({"project": project, "version": v["version_number"], "file": file["filename"]})
        common.append({"path": f"mods/{file['filename']}", "url": file["url"], "algorithm": "sha512", "hash": file["hashes"]["sha512"]})
    logging = mc.get("logging", {}).get("client")
    if logging:
        common.append(descriptor(logging["file"], f"logging/{logging['file']['id']}"))
    platforms = {}
    for platform in PLATFORMS:
        files, classpath, natives = [], [], []
        # Fabric entries come first, replacing any matching vanilla coordinates.
        libraries = {}
        for lib in mc["libraries"] + fabric["libraries"]:
            key = ":".join(lib["name"].split(":")[:2] + lib["name"].split(":")[3:])
            libraries[key] = lib
        for lib in libraries.values():
            if not allowed(lib.get("rules"), platform):
                continue
            artifact = lib.get("downloads", {}).get("artifact")
            if artifact is None and "downloads" not in lib:
                path = maven_path(lib["name"])
                url = lib.get("url", "https://libraries.minecraft.net/") + path
                h = lib.get("sha256")
                if h is None:
                    h = fetch(url + ".sha256").decode().strip().split()[0]
                artifact = {"path": path, "url": url, "sha256": h}
            if artifact:
                path = "libraries/" + artifact["path"]
                files.append(descriptor(artifact, path))
                classpath.append(path)
            classifier = lib.get("natives", {}).get(platform)
            if classifier:
                artifact = lib["downloads"]["classifiers"][classifier.replace("${arch}", "64")]
                path = "libraries/" + artifact["path"]
                files.append(descriptor(artifact, path))
                natives.append({"path": path, "exclude": lib.get("extract", {}).get("exclude", [])})
        classpath.append("versions/26.3/client.jar")
        java_major = mc["javaVersion"]["majorVersion"]
        releases = remote_json(f"https://api.adoptium.net/v3/assets/latest/{java_major}/hotspot?architecture=x64&image_type=jre&os={platform}&vendor=eclipse")
        jre = releases[0]["binary"]["package"]
        manifest = {"format": 1, "platform": platform, "minecraft": version, "fabric": loader,
                    "java": "runtime/bin/java.exe" if platform == "windows" else "runtime/bin/java",
                    "java_major": java_major, "main_class": fabric["mainClass"], "asset_index": ai["id"],
                    "classpath": classpath, "mods": [m["file"] for m in mods], "mod_versions": mods,
                    "jvm_args": arguments(mc["arguments"]["jvm"], platform) + arguments(fabric.get("arguments", {}).get("jvm", []), platform),
                    "game_args": arguments(mc["arguments"]["game"], platform) + arguments(fabric.get("arguments", {}).get("game", []), platform)}
        if logging:
            manifest["jvm_args"].append(logging["argument"].replace("${path}", "${bundle_directory}/logging/" + logging["file"]["id"]))
        platforms[platform] = {"files": files, "natives": natives, "manifest": manifest,
                               "runtime": {"url": jre["link"], "hash": jre["checksum"], "algorithm": "sha256", "name": jre["name"]}}
    write_json(ROOT / "bundle.lock.json", {"format": 1, "common": common, "platforms": platforms})
    print(f"Pinned Minecraft {version}, Fabric {loader}, Java {java_major}, {len(common)} common files", flush=True)


def download(item):
    CACHE.mkdir(parents=True, exist_ok=True)
    dest = CACHE / item["hash"]
    if dest.is_file() and hashlib.new(item["algorithm"], dest.read_bytes()).hexdigest() == item["hash"]:
        return dest
    ranged = "name" in item or item.get("path", "").startswith(("versions/", "libraries/", "mods/"))
    data = fetch_binary(item["url"]) if ranged else fetch(item["url"])
    if hashlib.new(item["algorithm"], data).hexdigest() != item["hash"]:
        raise ValueError(f"Checksum mismatch: {item['url']}")
    tmp = dest.with_suffix(f".{os.getpid()}.{threading.get_ident()}.tmp")
    tmp.write_bytes(data)
    tmp.replace(dest)
    return dest


def unpack_runtime(archive, target, name):
    target.mkdir(parents=True, exist_ok=True)
    if name.endswith(".zip"):
        with zipfile.ZipFile(archive) as z:
            for info in z.infolist():
                relative = PurePosixPath(*safe_path(info.filename).parts[1:])
                if info.is_dir() or str(relative) == ".":
                    continue
                path = target / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(z.read(info))
    else:
        with tarfile.open(archive) as t:
            for info in t.getmembers():
                relative = PurePosixPath(*safe_path(info.name).parts[1:])
                if info.isdir() or str(relative) == ".":
                    continue
                if not (info.isfile() or info.issym() or info.islnk()):
                    raise ValueError(f"Unsupported Java archive entry: {info.name}")
                # extractfile resolves archive links, creating regular files on both OSes.
                path = target / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                with t.extractfile(info) as source, path.open("wb") as out:
                    shutil.copyfileobj(source, out)
                path.chmod(info.mode & 0o777)


def patch_java_manifest(path):
    """Opt Java into UTF-8 on Windows 10 1903+, preserving PE resource offsets.

    Like Bazel's minimized JDK, keep Windows 10 compatibility and replace older
    supportedOS entries with the per-process UTF-8 setting. No machine changes.
    """
    data = path.read_bytes()
    if not data.startswith(b"MZ"):
        raise ValueError("Expected a Windows Java executable")
    start = data.find(b"<assembly ")
    end = data.find(b"</assembly>", start)
    if start < 0 or end < 0:
        raise ValueError("Java executable has no supported embedded manifest")
    end += len(b"</assembly>")
    original = data[start:end]
    ET.fromstring(original)
    if b"<activeCodePage" in original:
        if b">UTF-8</activeCodePage>" not in original:
            raise ValueError("Unexpected Java process code page")
        return
    win10 = b"8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a"
    changed = re.sub(rb"<supportedOS\b[^>]*(?:/>|></supportedOS>)",
                     lambda match: match[0] if win10 in match[0].lower() else b"", original)
    marker = b"</asmv3:windowsSettings>"
    if marker not in changed:
        raise ValueError("Unsupported Java manifest Windows settings")
    setting = b'<activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>'
    changed = changed.replace(marker, setting + marker, 1)
    ET.fromstring(changed)
    if len(changed) > len(original):
        raise ValueError("Insufficient room for Java UTF-8 process manifest")
    path.write_bytes(data[:start] + changed.ljust(len(original), b" ") + data[end:])


def prepare(args):
    lock = json.loads((ROOT / "bundle.lock.json").read_text())
    platform = lock["platforms"][args.platform]
    stage = ROOT / "build" / args.platform / "payload"
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    files = {f["path"]: f for f in lock["common"] + platform["files"]}
    count = 0
    def install(item):
        candidates = [ROOT / item["path"], ROOT / Path(item["path"]).name] if item["path"].startswith("mods/") else []
        if args.minecraft_cache:
            candidates.append(Path(args.minecraft_cache) / safe_path(item["path"]))
            if item["path"] == "versions/26.3/client.jar":
                candidates.append(Path(args.minecraft_cache) / "versions/26.3/26.3.jar")
        for existing in candidates:
            cached = CACHE / item["hash"]
            if not cached.exists() and existing.is_file():
                data = existing.read_bytes()
                if hashlib.new(item["algorithm"], data).hexdigest() == item["hash"]:
                    CACHE.mkdir(parents=True, exist_ok=True)
                    temp = cached.with_suffix(f".{os.getpid()}.{threading.get_ident()}.tmp")
                    temp.write_bytes(data)
                    temp.replace(cached)
        src = download(item)
        dest = stage / safe_path(item["path"])
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dest)
    with concurrent.futures.ThreadPoolExecutor(max_workers=24) as pool:
        jobs = [pool.submit(install, item) for item in files.values()]
        for job in concurrent.futures.as_completed(jobs):
            job.result()
            count += 1
            if count % 250 == 0:
                print(f"Verified {count}/{len(files)} files", flush=True)
    print("Preparing bundled Java runtime…", flush=True)
    unpack_runtime(download(platform["runtime"]), stage / "runtime", platform["runtime"]["name"])
    if args.platform == "windows":
        for executable in ("java.exe", "javaw.exe"):
            patch_java_manifest(stage / "runtime/bin" / executable)
    for native in platform["natives"]:
        with zipfile.ZipFile(stage / native["path"]) as z:
            for info in z.infolist():
                if info.is_dir() or any(info.filename.startswith(e) for e in native["exclude"]):
                    continue
                dest = stage / "natives" / "java" / safe_path(info.filename)
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(z.read(info))
    write_json(stage / "manifest.json", platform["manifest"])
    receipt = {}
    for file in sorted(stage.rglob("*")):
        if file.is_file():
            receipt[file.relative_to(stage).as_posix()] = hashlib.sha256(file.read_bytes()).hexdigest()
    write_json(stage / "checksums.json", receipt)
    archive = stage.parent / "payload.tar.gz"
    print("Compressing payload…", flush=True)
    with archive.open("wb") as out, gzip.GzipFile(fileobj=out, mode="wb", compresslevel=9, mtime=0) as gz, tarfile.open(fileobj=gz, mode="w|") as tar:
        for file in sorted(stage.rglob("*")):
            if file.is_file():
                info = tar.gettarinfo(str(file), file.relative_to(stage).as_posix())
                info.uid = info.gid = info.mtime = 0
                info.uname = info.gname = ""
                info.mode = 0o755 if file.stat().st_mode & 0o111 else 0o644
                with file.open("rb") as source:
                    tar.addfile(info, source)
    print(f"Prepared {archive} ({archive.stat().st_size / 1024**2:.1f} MiB)", flush=True)


def pack(args):
    source, payload, output = Path(args.binary), Path(args.payload), Path(args.output)
    if source.resolve() == output.resolve():
        raise ValueError("Use a different output path from the input binary")
    # A package must match its binary's target. Inspect PE/ELF headers.
    with gzip.open(payload, "rb") as gz, tarfile.open(fileobj=gz, mode="r|") as tar:
        manifest = None
        for item in tar:
            if item.name == "manifest.json":
                manifest = json.load(tar.extractfile(item))
                break
    if not manifest:
        raise ValueError("Payload has no manifest")
    with source.open("rb") as f:
        header = f.read(64)
    expected = b"MZ" if manifest["platform"] == "windows" else b"\x7fELF"
    if not header.startswith(expected):
        raise ValueError("Binary and payload platforms do not match")
    output.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    with source.open("rb") as binary, payload.open("rb") as data, output.open("wb") as out:
        shutil.copyfileobj(binary, out)
        while chunk := data.read(1024 * 1024):
            digest.update(chunk)
            out.write(chunk)
        out.write(struct.pack("<Q", payload.stat().st_size) + digest.digest() + MAGIC)
    output.chmod(0o755)
    checksum = hashlib.sha256()
    with output.open("rb") as f:
        while chunk := f.read(1024 * 1024):
            checksum.update(chunk)
    output.with_name(output.name + ".sha256").write_text(f"{checksum.hexdigest()}  {output.name}\n")
    print(f"Created {output} ({output.stat().st_size / 1024**2:.1f} MiB)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="command", required=True)
    subs.add_parser("resolve").set_defaults(run=resolve)
    p = subs.add_parser("prepare")
    p.add_argument("--platform", choices=PLATFORMS, required=True)
    p.add_argument("--minecraft-cache", help="Reuse verified files from an existing .minecraft folder")
    p.set_defaults(run=prepare)
    p = subs.add_parser("pack")
    p.add_argument("--binary", required=True)
    p.add_argument("--payload", required=True)
    p.add_argument("--output", required=True)
    p.set_defaults(run=pack)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
