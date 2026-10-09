#!/usr/bin/env python3
"""Exercise server configuration installation without network or user data."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class ServerConfigTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="memcan-setup-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.server = self.root / "server"
        self.server.mkdir()
        self.compose = self.server / "docker-compose.yml"
        self.fallback = self.server / "traefik/memcan-unavailable.yml"
        self.fixture = self.root / "compose.yml"
        self.fixture.write_text((ROOT / "docker-compose.yml").read_text())

    def install(self, fail=False):
        # Load the real installer functions, skipping its CLI/main entry point.
        source = (ROOT / "setup.sh").read_text().split("# --- Main ---")[0]
        result = subprocess.run(
            [
                "bash",
                "-c",
                source
                + r"""
SERVER_DIR="$TEST_SERVER"
CLI_ENV_DIR="$TEST_SERVER/cli"
merge_env() { :; }
openssl() { printf '%s\n' 'test-key'; }
curl() {
    local output url
    while [ "$#" -gt 0 ]; do
        case "$1" in
            -o) output="$2"; shift 2 ;;
            -*) shift ;;
            *) url="$1"; shift ;;
        esac
    done
    case "$url" in
        */v2.0.2/docker-compose.yml) cp "$TEST_COMPOSE" "$output" ;;
        */v2.0.2/traefik/memcan-unavailable.yml)
            if [ "$TEST_FAIL" = 1 ]; then
                printf 'partial download' > "$output"
                return 22
            fi
            cp "$TEST_FALLBACK" "$output" ;;
        *) return 99 ;;
    esac
}
setup_server v2.0.2
""",
            ],
            env=dict(
                os.environ,
                TEST_SERVER=str(self.server),
                TEST_COMPOSE=str(self.fixture),
                TEST_FALLBACK=str(ROOT / "traefik/memcan-unavailable.yml"),
                TEST_FAIL=str(int(fail)),
            ),
            capture_output=True,
            text=True,
        )
        return result

    def test_failed_download_does_not_publish_incomplete_install(self):
        self.assertNotEqual(self.install(fail=True).returncode, 0)
        self.assertFalse(self.compose.exists())
        self.assertFalse(self.fallback.exists())

    def test_failed_download_preserves_installed_configuration(self):
        self.compose.write_text("existing compose\n")
        self.fallback.parent.mkdir()
        self.fallback.write_text("existing fallback\n")
        self.assertNotEqual(self.install(fail=True).returncode, 0)
        self.assertEqual(self.compose.read_text(), "existing compose\n")
        self.assertEqual(self.fallback.read_text(), "existing fallback\n")

    def test_success_installs_both_files_from_selected_release(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.compose.read_text(), self.fixture.read_text())
        self.assertEqual(
            self.fallback.read_text(),
            (ROOT / "traefik/memcan-unavailable.yml").read_text(),
        )

    def test_legacy_compose_does_not_need_fallback_download(self):
        self.fixture.write_text("services: {}\n")
        result = self.install(fail=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.compose.read_text(), self.fixture.read_text())
        self.assertFalse(self.fallback.exists())


if __name__ == "__main__":
    unittest.main()
