#!/usr/bin/env python3

"""
Extract the flasher stubs from esptool.py.

Usage: IDF_PATH=/path/to/esp-idf ./stub.py
"""

import base64
import zlib
import os
import os.path
from struct import pack
import sys

if 'IDF_PATH' not in os.environ:
    print("Set the IDF_PATH environment variable (run the export.sh script in the esp-idf directory)",
          file=sys.stderr)
    sys.exit(1)

class Module(object):
    """
    Satisfies all the requirements for the two serial modules.
    """
    def __init__(self, defs = None):
        self.__dict__ = defs or dict()
    def __getattr__(self, name):
        return self.__dict__[name]

sys.modules['serial'] = Module()
sys.modules['serial.tools'] = Module({'list_ports': lambda: None })
sys.path.append(os.path.join(os.environ['IDF_PATH'], 'components/esptool_py/esptool'))
try:
    import esptool
except ImportError:
    print("Failed to load esptool.py", file=sys.stderr)
    sys.exit(1)


MAGIC = b'STUB'

def image(stub, type):
    """
    All values are little endian.

    00:  Magic value: 'STUB'
    04:  Chip type
    08:  Entry address
    0C:  Text start
    10:  Text length
    14:  Text
    n:   Data start
    n+4: Data length
    n+8: Data
    """
    # entry text_start text_len text data_start data_len data
    entry = stub['entry']
    text_start = stub['text_start']
    text = stub['text']
    data_start = stub['data_start']
    data = stub['data']
    im = bytearray(MAGIC)
    im.extend(pack('<IIII', type, entry, text_start, len(text)))
    im.extend(text)
    im.extend(pack('<II', data_start, len(data)))
    im.extend(data)
    return im

with open('esp8266_stub.bin', 'wb') as f:
    f.write(image(esptool.ESP8266ROM.STUB_CODE, 0x10000))
with open('esp32_stub.bin', 'wb') as f:
    f.write(image(esptool.ESP32ROM.STUB_CODE, 0))
with open('esp32s2_stub.bin', 'wb') as f:
    f.write(image(esptool.ESP32S2ROM.STUB_CODE, 2))
with open('esp32s3_stub.bin', 'wb') as f:
    f.write(image(esptool.ESP32S3ROM.STUB_CODE, 9))
with open('esp32c3_stub.bin', 'wb') as f:
    f.write(image(esptool.ESP32C3ROM.STUB_CODE, 5))

