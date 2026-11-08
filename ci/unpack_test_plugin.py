from pathlib import Path
import sys
from package import extract

root=Path(__file__).resolve().parents[1]
platform=sys.argv[1]
extract(root/'artifacts/plugins'/f'yoodesk-vnc-{platform}.zip',root/'artifacts/test-vnc')
