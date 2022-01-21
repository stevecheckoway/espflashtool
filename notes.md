# Detecting the chip.

## ESP8266
Some ESP8266 apparently boot with the serial port not set to 115200 baud. See
[https://docs.espressif.com/projects/esptool/en/latest/esp8266/advanced-topics/serial-protocol.html#initial-synchronisation](here)
for details.

Therefore we cannot try to find the text "waiting for download\r\n" and must
just wait for the serial port read to time out.

My ESP8266's magic number is 0xFFF0C101.

## ESP32

My ESP32's magic number is 0x00F01D83.

## ESP32S2

My ESP32S2's magic number is 0x000007C6.

## ESP32S3

## ESP32C3

My ESP32C3's magic number is 0x1B31506F.
