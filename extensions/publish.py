"""Builds open-source extensions from their own source and publishes them to
I-IDE's extension store (the server's /api/extensions).

Each extension is taken at the commit the community extension index
(github.com/zed-industries/extensions) pins, built with I-IDE's own
extension builder (crates/extension_cli in the i-ide repo), and kept with its
licence. Extensions without a licence file are skipped.

    python3 extensions/publish.py build                 # every extension in catalog.txt
    python3 extensions/publish.py build java ruby       # just these
    python3 extensions/publish.py bundle               # copy built packages into extensions/packages
    EXTENSIONS_PUBLISH_TOKEN=... python3 extensions/publish.py upload https://i-ide.dev

`bundle` puts the packages in the repo, where the server loads them at startup:
commit and push, and the live store has them without any token.

Built packages are kept in ~/.cache/i-ide-extensions/out/<id>/ so they can be
uploaded to several servers (a local demo and the real one) without rebuilding.
"""
import base64, json, os, re, subprocess, sys, urllib.request

try:
    import tomllib
except ModuleNotFoundError:  # Python before 3.11
    tomllib = None
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
    text = (INDEX / "extensions.toml").read_text()
    if tomllib:
        return tomllib.loads(text)
    # The index is flat: [name] sections of key = "value" lines.
    entries, current = {}, None
    for line in text.splitlines():
        line = line.split("#")[0].strip()
        if match := re.fullmatch(r"\[([^\]]+)\]", line):
            current = entries.setdefault(match.group(1).strip('"'), {})
        elif current is not None and (match := re.fullmatch(r'([A-Za-z_]+)\s*=\s*"([^"]*)"', line)):
            current[match.group(1)] = match.group(2)
    return entries


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


def bundle():
    target = HERE / "packages"
    count = 0
    for package in sorted(OUT.iterdir()) if OUT.exists() else []:
        if (package / "archive.tar.gz").exists() and (package / "manifest.json").exists():
            (target / package.name).mkdir(parents=True, exist_ok=True)
            for name in ("archive.tar.gz", "manifest.json"):
                (target / package.name / name).write_bytes((package / name).read_bytes())
            count += 1
    print(f"bundled {count} packages into {target}")


if __name__ == "__main__":
    command, *rest = sys.argv[1:] or ["help"]
    if command == "build":
        build(rest or catalog())
    elif command == "bundle":
        bundle()
    elif command == "upload" and rest:
        upload(rest[0], set(rest[1:]))
    else:
        print(__doc__)
