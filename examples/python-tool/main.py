import sys

import idna

print(f"hello from Python {sys.version.split()[0]}, built by Neki: {idna.encode('bücher.example').decode()}")
