import time
import os

while True:
    print("Opening file...")
    try:
        f = open("/dev/null", "r")
        f.close()
    except:
        pass
    time.sleep(2)
