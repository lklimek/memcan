#!/usr/bin/env python3
"""Exercise the Compose proxy against a disposable, health-controlled backend."""

import json
import os
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import uuid


ROOT = Path(__file__).resolve().parents[1]
TOKEN = "memcan-proxy-test-only"


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True).strip()


def main():
    env = dict(os.environ, MEMCAN_API_KEY=TOKEN, OLLAMA_API_KEY=TOKEN)
    config = json.loads(
        subprocess.check_output(
            [
                "docker",
                "compose",
                "--env-file",
                "/dev/null",
                "-f",
                str(ROOT / "docker-compose.yml"),
                "config",
                "--format",
                "json",
            ],
            env=env,
            text=True,
        )
    )
    proxy_config = config["services"]["traefik"]
    name = f"memcan-proxy-test-{uuid.uuid4().hex[:12]}"
    backend, proxy = f"{name}-backend", f"{name}-proxy"
    created = []
    network_created = False
    try:
        docker("network", "create", name)
        network_created = True
        labels = dict(config["services"]["memcan"]["labels"])
        labels.update(
            {
                "traefik.instance": name,
                "traefik.http.services.memcan.loadbalancer.server.port": "8080",
                "traefik.http.middlewares.memcan-ipallow.ipallowlist.sourcerange": "0.0.0.0/0",
            }
        )
        args = [
            "create",
            "--name",
            backend,
            "--network",
            name,
            "--health-cmd",
            "test -f /tmp/ready",
            "--health-interval",
            "1s",
            "--health-start-period",
            "1s",
            "--health-retries",
            "1",
        ]
        for key, value in labels.items():
            args += ["--label", f"{key}={value}"]
        backend_args = [
            *args,
            proxy_config["image"],
            "--entrypoints.fixture.address=:8080",
            "--ping=true",
            "--ping.entrypoint=fixture",
        ]

        args = [
            "run",
            "-d",
            "--name",
            proxy,
            "--network",
            name,
            "-p",
            "127.0.0.1::8190",
        ]
        for volume in proxy_config["volumes"]:
            args += ["-v", f"{volume['source']}:{volume['target']}:ro"]
        command = [
            arg
            for arg in proxy_config["command"]
            if not arg.startswith(
                ("--providers.docker.network=", "--providers.docker.constraints=")
            )
        ]
        command += [
            f"--providers.docker.network={name}",
            f"--providers.docker.constraints=Label(`traefik.instance`,`{name}`)",
        ]
        created.append(proxy)
        docker(*args, proxy_config["image"], *command)
        port = docker("port", proxy, "8190/tcp").rsplit(":", 1)[1]
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

        def status(path="/mcp", authenticated=True):
            request = urllib.request.Request(f"http://127.0.0.1:{port}{path}")
            if authenticated:
                request.add_header("Authorization", f"Bearer {TOKEN}")
            try:
                with opener.open(request, timeout=1) as response:
                    return response.status
            except urllib.error.HTTPError as error:
                return error.code
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                return 0

        def expect(stage, expected, path="/mcp"):
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                actual = status(path)
                if actual == expected:
                    print(f"{stage}: {actual}", flush=True)
                    return
                time.sleep(0.1)
            raise AssertionError(f"{stage}: expected {expected}, got {actual}")

        expect("backend absent", 503)
        created.append(backend)
        docker(*backend_args)
        docker("start", backend)
        expect("backend not healthy", 503)
        docker("exec", backend, "touch", "/tmp/ready")
        expect("backend healthy", 200, "/ping")
        assert status("/ping", authenticated=False) in {401, 403}, "auth bypassed"
        docker("exec", backend, "rm", "/tmp/ready")
        expect("backend unhealthy", 503)
        docker("stop", "-t", "1", backend)
        expect("backend stopped", 503)
        docker("start", backend)
        docker("exec", backend, "touch", "/tmp/ready")
        expect("backend recovered", 200, "/ping")
        docker("rm", "-f", backend)
        expect("backend removed", 503)
    finally:
        for container in reversed(created):
            subprocess.run(
                ["docker", "rm", "-f", container],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        if network_created:
            subprocess.run(
                ["docker", "network", "rm", name],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )


if __name__ == "__main__":
    main()
