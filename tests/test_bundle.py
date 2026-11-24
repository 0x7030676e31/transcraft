import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("bundle", Path(__file__).resolve().parents[1] / "tools/bundle.py")
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)


class BundleTests(unittest.TestCase):
    def test_java_manifest_enables_utf8_without_moving_pe_resources(self):
        manifest = b'''<assembly xmlns="urn:schemas-microsoft-com:asm.v1" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3" manifestVersion="1.0"><trustInfo><requestedExecutionLevel level="asInvoker"/></trustInfo><asmv3:windowsSettings></asmv3:windowsSettings><compatibility><supportedOS Id="{e2011457-1546-43c5-a5fe-008deee3d3f0}"></supportedOS><supportedOS Id="{35138b9a-5d96-4fbd-8e2d-a2440225f93a}"></supportedOS><supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"></supportedOS></compatibility></assembly>'''
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "java.exe"
            original = b"MZprefix" + manifest + b"\x00unchanged-suffix"
            path.write_bytes(original)
            bundle.patch_java_manifest(path)
            changed = path.read_bytes()
            self.assertEqual(len(changed), len(original))
            self.assertTrue(changed.endswith(b"\x00unchanged-suffix"))
            self.assertIn(b">UTF-8</activeCodePage>", changed)
            self.assertIn(b'level="asInvoker"', changed)
            self.assertIn(b"8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a", changed)
            bundle.patch_java_manifest(path)
            self.assertEqual(path.read_bytes(), changed)

    def test_ranged_download_reassembles_exact_bytes(self):
        import re
        data = b"A" * (1024 * 1024) + b"B" * (1024 * 1024) + b"tail"
        class Response(io.BytesIO):
            status = 206
        def open_range(req, **kwargs):
            start, end = map(int, re.fullmatch(r"bytes=(\d+)-(\d+)", req.get_header("Range")).groups())
            end = min(end, len(data) - 1)
            result = Response(data[start:end + 1])
            result.headers = {"Content-Range": f"bytes {start}-{end}/{len(data)}"}
            return result
        with patch.object(bundle.urllib.request, "urlopen", side_effect=open_range):
            self.assertEqual(bundle.fetch_binary("https://example.invalid/large"), data)

    def test_rules_keep_only_target_natives_and_resolution(self):
        self.assertTrue(bundle.allowed(None, "linux"))
        rules = [{"action": "allow", "os": {"name": "windows"}}]
        self.assertFalse(bundle.allowed(rules, "linux"))
        self.assertTrue(bundle.allowed(rules, "windows"))
        rules.append({"action": "disallow", "os": {"arch": "x86_64"}})
        self.assertFalse(bundle.allowed(rules, "windows"))
        for key, expected in [("is_demo_user", False), ("has_custom_resolution", True), ("is_quick_play_realms", False)]:
            self.assertEqual(bundle.allowed([{"action": "allow", "features": {key: True}}], "linux"), expected)

    def test_paths_are_safe_on_both_platforms(self):
        for name in ["../outside", "/absolute", "C:/windows", "a\\b", "a/../../b", ""]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                bundle.safe_path(name)

    def test_download_rejects_bad_checksum(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(bundle, "CACHE", Path(temp)), patch.object(bundle, "fetch", return_value=b"wrong"):
            with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
                bundle.download({"hash": "0" * 64, "algorithm": "sha256", "url": "https://example.invalid/file"})
            self.assertEqual(list(Path(temp).iterdir()), [])

    def test_runtime_links_become_regular_files(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "jre.tar.gz"
            with tarfile.open(archive, "w:gz") as t:
                file = tarfile.TarInfo("jre/bin/java")
                file.size, file.mode = 4, 0o755
                t.addfile(file, io.BytesIO(b"java"))
                link = tarfile.TarInfo("jre/bin/javaw")
                link.type, link.linkname = tarfile.SYMTYPE, "java"
                t.addfile(link)
            bundle.unpack_runtime(archive, root / "out", "jre.tar.gz")
            self.assertEqual((root / "out/bin/javaw").read_bytes(), b"java")
            self.assertFalse((root / "out/bin/javaw").is_symlink())

    def test_lock_has_verified_complete_platforms(self):
        lock = json.loads((bundle.ROOT / "bundle.lock.json").read_text())
        for platform in ("linux", "windows"):
            p = lock["platforms"][platform]
            self.assertEqual(p["manifest"]["platform"], platform)
            files = {f["path"] for f in lock["common"] + p["files"]}
            self.assertTrue(set(p["manifest"]["classpath"]) <= files)
            self.assertEqual(len(p["manifest"]["mods"]), 4)
            for f in lock["common"] + p["files"] + [p["runtime"]]:
                self.assertTrue(f["url"].startswith("https://"))
                self.assertRegex(f["hash"], r"^[0-9a-f]+$")
                self.assertEqual(len(f["hash"]), {"sha1": 40, "sha256": 64, "sha512": 128}[f["algorithm"]])


if __name__ == "__main__":
    unittest.main()
