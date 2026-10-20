"""Writes the package: share/hello/greeting.txt."""

import os
import sys

out = sys.argv[1]
os.makedirs(os.path.join(out, "share", "hello"))
with open(os.path.join(out, "share", "hello", "greeting.txt"), "w", newline="\n") as f:
    f.write("hello from a Neki build\n")
