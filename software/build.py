#!/usr/bin/env python3
"""Builds the packages Software offers (software/packages.toml) into out/software:

    out/software/index            what there is: one paragraph per package ("Key: value" lines)
    out/software/NAME.tar.gz      a package: its files, as they go under /, plus .package/
    out/software/icons/NAME.png   its picture, for the catalog

Each package is made from Ubuntu 24.04's .debs, downloaded with `apt-get download` into
~/.cache/kernelproject2/software (nothing is installed on this machine). The libraries its programs
need come along, except the ones KernelProject2 already has (worked out against the staged root
build.sh made, with our SDL 2 standing in for Ubuntu's).

A package's .package/ holds:
    info    its paragraph from the index
    links   the paths it puts on the system, one per line: when it's installed, each becomes a
            symlink into the package's own folder. A folder no one else uses is one link; files in
            shared folders (/usr/games, the library folder, ...) are linked one by one.
    icon.png

    python3 software/build.py       (after build.sh; run inside Linux like it)
"""

import gzip
import io
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib

here = os.path.dirname(os.path.abspath(__file__))
root = os.path.dirname(here)
out = os.path.join(root, "out", "software")
stage = os.environ.get("KP2_STAGE", os.path.expanduser("~/.cache/kernelproject2/root"))
cache = os.path.expanduser("~/.cache/kernelproject2/software")
icons = os.path.join(root, "rootfs", "usr", "share", "icons", "oxygen", "48x48")
libdir = "lib/x86_64-linux-gnu"

# Dependencies never worth fetching: what KernelProject2 has or replaces (the C library, SDL 2), what
# it can't use (X11, Wayland, GPUs, sound servers), and big optional data (MIDI sound fonts).
SKIP = re.compile(r"^(libc6|libgcc-s1|libstdc\+\+6|libsdl2-2\.0-0|libgl|libegl|libgles|libglx|libgbm|mesa|"
                  r"libx11|libxcb|libxext|libxrandr|libxi|libxcursor|libxfixes|libxss|libxkb|libwayland|"
                  r"libdecor|libdrm|libpulse|libasound2|libpipewire|libdbus|libsystemd|libudev|libibus|"
                  r"libvulkan|x11-|xkb-data|libjack|debconf|cdebconf|dpkg|perl|fonts-|libllvm|"
                  r".*soundfont.*|wamerican.*|dictionaries-common|miscfiles|emacsen-common)")
# Parts of a .deb nobody here reads.
PRUNE = ("usr/share/man", "usr/share/bash-completion", "etc/init.d", "usr/lib/systemd", "usr/share/info", "usr/share/menu", "usr/share/lintian", "usr/share/metainfo",
         "usr/share/applications", "usr/share/bug", "usr/share/doc-base")


def sh(*cmd, cwd=None, env=None):
    return subprocess.run(cmd, cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout


def fail(msg):
    sys.exit(f"software/build.py: {msg}")


def installed_here(deb):
    return subprocess.run(["dpkg", "-s", deb], capture_output=True).returncode == 0


def fetch(debs):
    """Downloads these .debs (once), and returns their paths."""
    os.makedirs(f"{cache}/debs", exist_ok=True)
    paths = []
    for deb in debs:
        have = [f for f in os.listdir(f"{cache}/debs") if f.startswith(deb + "_") and f.endswith(".deb")]
        if not have:
            try:
                sh("apt-get", "download", "-q", deb, cwd=f"{cache}/debs")
            except subprocess.CalledProcessError as e:
                fail(f"can't download {deb}: {e.stderr.strip()}")
            have = [f for f in os.listdir(f"{cache}/debs") if f.startswith(deb + "_") and f.endswith(".deb")]
        paths.append(f"{cache}/debs/{sorted(have)[-1]}")
    return paths


def dependencies(debs):
    """The packages these depend on, all the way down, that might hold a library we need."""
    text = sh("apt-cache", "depends", "--recurse", "--no-recommends", "--no-suggests", "--no-conflicts",
              "--no-breaks", "--no-replaces", "--no-enhances", "--no-pre-depends", *debs)
    names = {l for l in text.splitlines() if l and not l.startswith((" ", "<"))}
    return sorted(n for n in names - set(debs) if not SKIP.match(n) and not installed_here(n))


def unpacked(deb_path):
    """A .deb's files, unpacked once into the cache."""
    where = f"{cache}/unpacked/{os.path.basename(deb_path)[:-4]}"
    if not os.path.isdir(where):
        os.makedirs(where + ".tmp", exist_ok=True)
        sh("dpkg-deb", "-x", deb_path, where + ".tmp")
        os.rename(where + ".tmp", where)
    return where


def is_elf(path):
    if os.path.islink(path) or not os.path.isfile(path):
        return False
    with open(path, "rb") as f:
        return f.read(4) == b"\x7fELF"


def in_stage(rel):
    return os.path.lexists(os.path.join(stage, rel))


def has_library(name):
    return any(in_stage(f"{d}/{name}") for d in ("lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu", "lib64", "lib", "usr/lib"))


def files_under(tree):
    """Every file and symlink under tree, relative to it."""
    found = []
    for dirpath, dirnames, filenames in os.walk(tree):
        for name in filenames + [d for d in dirnames if os.path.islink(os.path.join(dirpath, d))]:
            found.append(os.path.relpath(os.path.join(dirpath, name), tree))
    return sorted(found)


def build(name, spec, sdl, built):
    tree = f"{cache}/build/{name}"
    shutil.rmtree(tree, ignore_errors=True)
    os.makedirs(tree)
    debs = fetch(spec["debs"])
    version = sh("dpkg-deb", "-f", debs[0], "Version").strip()
    for deb in debs:
        src = unpacked(deb)
        shutil.copytree(src, tree, symlinks=True, dirs_exist_ok=True)

    # Just what's wanted: the files asked for (or everything but manuals and menus), and the
    # copyright notices whatever happens.
    keep_only = spec.get("only")
    for rel in files_under(tree):
        notice = rel.startswith("usr/share/doc/") and rel.endswith("/copyright")
        drop = (rel not in keep_only and not notice) if keep_only else (
            rel.startswith(PRUNE) or (rel.startswith("usr/share/doc/") and not notice and not os.path.islink(os.path.join(tree, rel))))
        if not drop and in_stage(rel) and not os.path.isdir(os.path.join(stage, rel)):
            print(f"    {name}: {rel} is already in the system; leaving it out")
            drop = True
        if drop:
            os.remove(os.path.join(tree, rel))
    for dirpath, _, _ in sorted(os.walk(tree, topdown=False)):
        if dirpath != tree and not os.listdir(dirpath) and not os.path.islink(dirpath):
            os.rmdir(dirpath)
    for rel, text in spec.get("files", {}).items():
        path = os.path.join(tree, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
        os.chmod(path, 0o755 if text.startswith("#!") else 0o664)

    # Libraries go where the system keeps its own (/lib/x86_64-linux-gnu), which the dynamic
    # linker searches; folders of plugins in /usr/lib/x86_64-linux-gnu stay where they are.
    multiarch = os.path.join(tree, "usr", libdir)
    if os.path.isdir(multiarch):
        os.makedirs(os.path.join(tree, libdir), exist_ok=True)
        for entry in os.listdir(multiarch):
            here_ = os.path.join(multiarch, entry)
            if os.path.islink(here_) or os.path.isfile(here_):
                os.rename(here_, os.path.join(tree, libdir, entry))
        if not os.listdir(multiarch):
            os.removedirs(multiarch)

    # Packages it depends on (built already) provide libraries for it too.
    dep_trees = [built[d][0] for d in spec.get("depends", [])]

    # Its own build step, with build-only .debs (headers, compilers) unpacked for it.
    if "script" in spec:
        tools = [unpacked(deb) for deb in fetch(spec.get("tools", []))]
        env = dict(os.environ, TREE=tree, SDL_BUILD=os.path.expanduser("~/.cache/kernelproject2/sdl2/build"),
                   TOOLS=" ".join(tools), DEP_TREES=" ".join(dep_trees))
        result = subprocess.run(["bash", os.path.join(here, spec["script"])], env=env, capture_output=True, text=True)
        if result.returncode != 0:
            fail(f"{name}: {spec['script']} failed:\n{result.stdout}{result.stderr}")

    # The libraries its programs need that the system doesn't have, from this machine or from the
    # dependencies' .debs.
    dep_dirs = []
    for deb in fetch(dependencies(spec["debs"])):
        src = unpacked(deb)
        for d in ("lib/x86_64-linux-gnu", "usr/lib/x86_64-linux-gnu"):
            if os.path.isdir(f"{src}/{d}"):
                dep_dirs.append(f"{src}/{d}")
    own_dirs = sorted({os.path.dirname(os.path.join(tree, r)) for r in files_under(tree) if ".so" in os.path.basename(r)})
    provided = [os.path.join(t, libdir) for t in dep_trees]
    env = dict(os.environ, LD_LIBRARY_PATH=":".join([sdl, *own_dirs, *provided, *dep_dirs]))
    for rel in files_under(tree):
        path = os.path.join(tree, rel)
        if not is_elf(path):
            continue
        for line in sh("ldd", path, env=env).splitlines():
            line = line.strip()
            if "not found" in line:
                fail(f"{name}: {rel} needs {line.split()[0]}, and none of its dependencies has it")
            m = re.match(r"(\S+) => (/\S+)", line)
            if not m:
                continue
            soname, lib = m.groups()
            if lib.startswith(tree) or has_library(soname) or any(lib.startswith(t) for t in dep_trees):
                continue
            dest = os.path.join(tree, libdir, soname)
            if not os.path.exists(dest):
                os.makedirs(os.path.dirname(dest), exist_ok=True)
                shutil.copy(os.path.realpath(lib), dest)
                os.chmod(dest, 0o644)
    return tree, version


def stage_paths():
    """Every path in the staged root (folders included), relative to it."""
    paths = set()
    for dirpath, dirnames, filenames in os.walk(stage):
        for n in dirnames + filenames:
            paths.add(os.path.relpath(os.path.join(dirpath, n), stage))
    return paths


def same_file(a, b):
    if os.path.islink(a) or os.path.islink(b):
        return os.path.islink(a) and os.path.islink(b) and os.readlink(a) == os.readlink(b)
    with open(a, "rb") as fa, open(b, "rb") as fb:
        return fa.read() == fb.read()


def links_for(name, files, trees, base):
    """The fewest paths that, linked into this package, put all its files in place: the shallowest
    folder of each file's that the system doesn't have and no other package uses, else the file.
    A file another package has too (a library both need) must be the same in both: whichever is
    installed first provides it, and `pkg remove` hands it over to another when that one goes."""
    taken = {}
    for other, (tree, _) in trees.items():
        if other == name:
            continue
        for rel in files_under(tree):
            parts = rel.split("/")
            for i in range(1, len(parts) + 1):
                taken.setdefault("/".join(parts[:i]), []).append(os.path.join(tree, rel) if i == len(parts) else None)
    mine = trees[name][0]
    result = []
    for rel in files:
        parts = rel.split("/")
        for i in range(1, len(parts) + 1):
            head = "/".join(parts[:i])
            if head in base:
                continue
            shared = taken.get(head)
            if shared and i == len(parts):
                if not all(p and same_file(p, os.path.join(mine, rel)) for p in shared):
                    fail(f"{name}: {rel} is in another package too, and isn't the same there")
            elif shared:
                continue
            if head not in result:
                result.append(head)
            break
        else:
            fail(f"{name}: {rel} is in the system already")
    return result


def icon(spec, tree):
    pic = spec["icon"]
    path = os.path.join(icons, pic[7:] + ".png") if pic.startswith("oxygen:") else os.path.join(tree, pic)
    if not os.path.isfile(path):
        fail(f"no icon at {path}")
    return path


def paragraph(fields):
    return "".join(f"{k}: {v}\n" for k, v in fields)


def write_package(name, tree, info, links, icon_path):
    """NAME.tar.gz: the tree, plus .package/, owned by root, in a fixed order (same input, same bytes)."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.GNU_FORMAT) as tar:
        def add(arcname, data=None, path=None):
            if path is not None:
                ti = tar.gettarinfo(path, arcname)
            else:
                ti = tarfile.TarInfo(arcname)
                ti.size, ti.mode = len(data), 0o644
            ti.uid = ti.gid = 0
            ti.uname = ti.gname = "root"
            ti.mtime = 0
            if ti.isreg():
                with open(path, "rb") if path else io.BytesIO(data) as f:
                    tar.addfile(ti, f)
            else:
                tar.addfile(ti)
        add(".package/info", info.encode())
        add(".package/links", "".join(l + "\n" for l in links).encode())
        add(".package/icon.png", path=icon_path)
        for rel in files_under(tree):
            add(rel, path=os.path.join(tree, rel))
    data = gzip.compress(buf.getvalue(), compresslevel=9, mtime=0)
    with open(os.path.join(out, name + ".tar.gz"), "wb") as f:
        f.write(data)
    return len(data)


def main():
    if not os.path.isdir(stage):
        fail(f"no staged root at {stage}: run build.sh first")
    with open(os.path.join(here, "packages.toml"), "rb") as f:
        catalog = tomllib.load(f)
    sdl = f"{cache}/sdl"
    sh("bash", os.path.join(root, "sdl2", "build.sh"), sdl)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(os.path.join(out, "icons"))

    built = {}
    for name, spec in catalog.items():
        print(f"    {name}")
        for dep in spec.get("depends", []):
            if dep not in built:
                fail(f"{name} depends on {dep}, which has to come before it in packages.toml")
        built[name] = build(name, spec, sdl, built)
    base = stage_paths()
    files = {name: files_under(tree) for name, (tree, _) in built.items()}

    index = []
    for name, spec in catalog.items():
        tree, version = built[name]
        links = links_for(name, files[name], built, base)
        installed = sum(os.lstat(os.path.join(tree, r)).st_size for r in files[name])
        fields = [("Package", name), ("Title", spec["title"]), ("Category", spec["category"]),
                  ("Version", version), ("Installed-Size", installed), ("Run", spec["run"]),
                  ("Terminal", "yes" if spec.get("terminal") else "no"), ("About", spec["about"])]
        if spec.get("depends"):
            fields.append(("Depends", ", ".join(spec["depends"])))
        pic = icon(spec, tree)
        size = write_package(name, tree, paragraph(fields), links, pic)
        shutil.copy(pic, os.path.join(out, "icons", name + ".png"))
        index.append(paragraph(fields[:4] + [("Size", size)] + fields[4:]))
        print(f"    {name} {version}: {size // 1024} KiB download, {installed // 1024} KiB installed, {len(links)} links")
    with open(os.path.join(out, "index"), "w") as f:
        f.write("\n".join(index))


main()
