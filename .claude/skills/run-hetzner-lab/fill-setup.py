"""fill-setup.py: write one server's setup file from this skill's cloud-init template.

The template's SSH_PUBLIC_KEY_PLACEHOLDER becomes the public half of ~/.ssh/estafette_lab,
the channel of the repository's rust-toolchain.toml replaces RUST_TOOLCHAIN_PLACEHOLDER,
and each line `FILE:<name>` becomes the content of that file from this skill's directory,
indented as the line was.

usage: python3 -I fill-setup.py <output path>
"""
import pathlib
import re
import sys

if len(sys.argv) != 2:
    print(__doc__)
    sys.exit(2)
here = pathlib.Path(__file__).resolve().parent
key = (pathlib.Path.home() / ".ssh" / "estafette_lab.pub").read_text().strip()
toolchain = re.search(r'^channel\s*=\s*"([^"]+)"', (here.parents[2] / "rust-toolchain.toml").read_text(), re.M)
assert toolchain, "no channel in rust-toolchain.toml"
text = (here / "cloud-init.template.yaml").read_text().replace("RUST_TOOLCHAIN_PLACEHOLDER", toolchain.group(1))
assert text.count("SSH_PUBLIC_KEY_PLACEHOLDER") == 1
text = text.replace("SSH_PUBLIC_KEY_PLACEHOLDER", key)
out = []
for line in text.splitlines():
    marker = re.fullmatch(r"(\s*)FILE:(\S+)", line)
    if marker:
        indent, name = marker.groups()
        out.extend(indent + body if body else "" for body in (here / name).read_text().splitlines())
    else:
        out.append(line)
pathlib.Path(sys.argv[1]).write_text("\n".join(out) + "\n")
print("written", sys.argv[1])
