"""Download an Isaac Sim USD asset plus everything it references.

    pip install usd-core
    python fetch_isaac_asset.py <url-of-root.usd> <out-dir>
"""
import os, re, sys, posixpath, urllib.request, urllib.error
from pxr import Sdf

url, out = sys.argv[1], sys.argv[2]
base, root = url.rsplit("/", 1)
seen = set()

def fetch(rel):
    rel = posixpath.normpath(rel)
    if rel in seen or rel.startswith(".."):
        return
    seen.add(rel)
    dst = os.path.join(out, rel)
    os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
    try:
        urllib.request.urlretrieve(f"{base}/{rel}", dst)
    except urllib.error.HTTPError as e:
        # Built-in MDL modules (OmniPBR.mdl, ...) ship with Omniverse, not the bucket.
        print(f"skip {rel} ({e.code})")
        return
    if not rel.endswith((".usd", ".usda", ".usdc")):
        return
    # Every @asset@ in the layer: sublayers, references, payloads AND
    # asset-valued attributes (textures), which the Sdf dependency lists miss.
    text = Sdf.Layer.FindOrOpen(dst).ExportToString()
    for dep in {m.split("<")[0] for m in re.findall(r"@([^@\n]+)@", text)}:
        if "://" not in dep and not dep.startswith("/"):
            fetch(posixpath.join(posixpath.dirname(rel), dep))

fetch(root)
print(f"{len(seen)} files -> {os.path.join(out, root)}")
