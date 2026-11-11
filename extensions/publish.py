"""Builds open-source extensions from their own source and publishes them to
I-IDE's extension store (the server's /api/extensions).

Each extension is taken at the commit the community extension index
(github.com/zed-industries/extensions) pins, built with I-IDE's own
extension builder (crates/extension_cli in the i-ide repo), and kept with its
licence. Extensions without a licence file are skipped.

    python3 extensions/publish.py build                 # every extension in catalog.txt
    python3 extensions/publish.py build java ruby       # just these
    EXTENSIONS_PUBLISH_TOKEN=... python3 extensions/publish.py upload https://i-ide.dev

Built packages are kept in ~/.cache/i-ide-extensions/out/<id>/ so they can be
uploaded to several servers (a local demo and the real one) without rebuilding.
"""
import base64, json, os, subprocess, sys, tomllib, urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
CACHE = Path.home() / ".cache" / "i-ide-extensions"
INDEX = CACHE / "index"
OUT = CACHE / "out"
I_IDE = Path(os.environ.get("I_IDE_REPO", Path.home() / "i-ide"))
BUILDER = I_IDE / "target" / "debug" / "zed-extension"
INDEX_REPO = "https://github.com/zed-industries/extensions"
LICENCE_NAMES = ("license", "licence", "copying")


def run(*args, cwd=None):
    subprocess.run(args, cwd=cwd, check=True)


def catalog():
    lines = (HERE / "catalog.txt").read_text().splitlines()
    return [line.split("#")[0].strip() for line in lines if line.split("#")[0].strip()]


def index_entries():
    if not (INDEX / ".git").exists():
        CACHE.mkdir(parents=True, exist_ok=True)
        run("git", "clone", "--depth", "1", "--filter=blob:none", INDEX_REPO, str(INDEX))
    else:
        run("git", "pull", "--ff-only", "-q", cwd=INDEX)
    return tomllib.loads((INDEX / "extensions.toml").read_text())


def has_licence(directory: Path):
    return any(child.name.lower().startswith(LICENCE_NAMES) for child in directory.iterdir())


def build(names):
    if not BUILDER.exists():
        print("building the extension builder…")
        run("cargo", "build", "-p", "extension_cli", cwd=I_IDE)
    entries = index_entries()
    built, skipped = [], []
    for name in names:
        entry = entries.get(name)
        if not entry:
            skipped.append((name, "not in the extension index"))
            continue
        submodule = entry["submodule"]
        run("git", "submodule", "update", "--init", "--depth", "1", submodule, cwd=INDEX)
        source = INDEX / submodule / entry.get("path", "")
        if not has_licence(INDEX / submodule) and not has_licence(source):
            skipped.append((name, "no licence file"))
            continue
        out = OUT / name
        subprocess.run(["rm", "-rf", str(out)])
        out.mkdir(parents=True)
        result = subprocess.run(
            [str(BUILDER), "--source-dir", str(source), "--output-dir", str(out), "--scratch-dir", str(CACHE / "scratch")],
            capture_output=True, text=True,
        )
        if result.returncode != 0 or not (out / "archive.tar.gz").exists():
            skipped.append((name, "build failed: " + (result.stderr.strip().splitlines() or ["?"])[-1][:200]))
            continue
        manifest = json.loads((out / "manifest.json").read_text())
        built.append(f"{name} {manifest['version']}")
        print(f"built {name} {manifest['version']}", flush=True)
    print(f"\n{len(built)} built, {len(skipped)} skipped")
    for name, why in skipped:
        print(f"  skipped {name}: {why}")


def upload(server, names):
    token = os.environ.get("EXTENSIONS_PUBLISH_TOKEN", "")
    if not token:
        sys.exit("Set EXTENSIONS_PUBLISH_TOKEN to the server's publish token.")
    server = server.rstrip("/")
    packages = sorted(path for path in OUT.iterdir() if (path / "archive.tar.gz").exists()) if OUT.exists() else []
    if names:
        packages = [path for path in packages if path.name in names]
    for package in packages:
        manifest = (package / "manifest.json").read_bytes()
        request = urllib.request.Request(
            f"{server}/api/extensions/{package.name}",
            data=(package / "archive.tar.gz").read_bytes(),
            method="POST",
            headers={
                "authorization": f"Bearer {token}",
                "content-type": "application/gzip",
                "x-extension-manifest": base64.b64encode(manifest).decode(),
            },
        )
        try:
            with urllib.request.urlopen(request) as response:
                print(f"published {package.name} ({response.status})", flush=True)
        except urllib.error.HTTPError as error:
            print(f"failed {package.name}: {error.code} {error.read().decode()[:200]}", flush=True)


if __name__ == "__main__":
    command, *rest = sys.argv[1:] or ["help"]
    if command == "build":
        build(rest or catalog())
    elif command == "upload" and rest:
        upload(rest[0], set(rest[1:]))
    else:
        print(__doc__)
