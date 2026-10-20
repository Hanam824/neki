import importlib.util
import sys

import idna
import markupsafe

print(f"hello from Python {sys.version.split()[0]}, built by Neki: {idna.encode('bücher.example').decode()}")
native = importlib.util.find_spec("markupsafe._speedups") is not None
print(f"markupsafe escaped {markupsafe.escape('<b>')} (native speedups: {'yes' if native else 'no'})")
