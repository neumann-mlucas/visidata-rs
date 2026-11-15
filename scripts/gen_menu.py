"""VisiData menu paths keyed by default key: `<vdrs keys>\t<menu path>` lines.

Usage: python3 scripts/gen_menu.py ../visidata/visidata > src/menu.tsv
"""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
CLASSES = {"BaseSheet", "Sheet", "TableSheet", "globalCommand"}
cmd_re = re.compile(r"\b(\w+)\.addCommand\(\s*(?:'([^']*)'|\"([^\"]*)\"|None)\s*,\s*'([^']+)'")
glob_re = re.compile(r"\bglobalCommand\(\s*'([^']*)'\s*,\s*'([^']+)'")
bind_re = re.compile(r"\b(\w+)\.bindkey\(\s*'([^']+)'\s*,\s*'([^']+)'")
menu_re = re.compile(r"addMenuItems\((?:'''|\"\"\")(.*?)(?:'''|\"\"\")", re.S)

keys = {}  # longname -> [keystroke]
menus = []  # (path, longname)
for f in sorted(root.rglob("*.py")):
    if "experimental" in f.parts or "apps" in f.parts:
        continue
    t = f.read_text(errors="replace")
    for cls, k1, k2, name in cmd_re.findall(t):
        k = k1 or k2
        if cls in CLASSES and k:
            keys.setdefault(name, []).append(k)
    for k, name in glob_re.findall(t):
        if k:
            keys.setdefault(name, []).append(k)
    for cls, k, name in bind_re.findall(t):
        if cls in CLASSES:
            keys.setdefault(name, []).append(k)
    for block in menu_re.findall(t):
        for line in block.splitlines():
            parts = [p.strip() for p in line.split(">")]
            if len(parts) >= 2 and parts[-1]:
                menus.append((" > ".join(parts[:-1]), parts[-1]))

NAMED = {"Enter": "<CR>", "Del": "<Del>", "Tab": "<Tab>", "Shift+Tab": "<S-Tab>",
         "Esc": "<Esc>", "Bksp": "<BS>", "KEY_BACKSPACE": "<BS>", "Space": "<Space>",
         "Home": "<Home>", "End": "<End>", "PgUp": "<PgUp>", "PgDn": "<PgDn>", "KEY_PPAGE": "<PgUp>", "KEY_NPAGE": "<PgDn>", " ": "<Space>",
         "Up": "<Up>", "Down": "<Down>", "Left": "<Left>", "Right": "<Right>",
         "KEY_UP": "<Up>", "KEY_DOWN": "<Down>", "KEY_LEFT": "<Left>", "KEY_RIGHT": "<Right>"}


def token(k):
    if k in NAMED:
        return NAMED[k]
    m = re.fullmatch(r"F(\d+)", k)
    if m:
        return f"<F{m[1]}>"
    m = re.fullmatch(r"(Ctrl|Alt|Shift)\+(.+)", k)
    if m:
        mod = {"Ctrl": "C", "Alt": "A", "Shift": "S"}[m[1]]
        rest = NAMED.get(m[2], m[2])
        rest = rest.strip("<>") if rest.startswith("<") else rest
        if mod == "C" and len(rest) == 1:
            rest = rest.lower()
        return f"<{mod}-{rest}>"
    if len(k) == 1:
        return None if k == "<" else k  # vdrs has no spelling for `<`
    return None


def vdrs(k):
    out = []
    k = k.replace(" ", "") if k.strip() else k
    while k[:1] in ("g", "z") and len(k) > 1:
        out.append(k[0])
        k = k[1:]
    t = token(k)
    return " ".join(out + [t]) if t else None


seen = set()
for path, name in menus:
    for k in keys.get(name, []):
        v = vdrs(k)
        if v and v not in seen:
            seen.add(v)
            print(f"{v}\t{path} > {name}")
