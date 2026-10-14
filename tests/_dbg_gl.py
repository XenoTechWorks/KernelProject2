import os
import sys
import time
sys.path.insert(0, "tests")
from machine import Machine

out = os.environ.get("OUT", "/tmp")
with Machine(software=True) as m:
    print(m.run("pkg get neverball", timeout=1800))
    print(m.run("LP_NUM_THREADS=0 neverball; echo EXIT=$?", timeout=60))
print("finished")
