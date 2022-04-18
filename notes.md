# Notes

## Detecting the chip

### Detecting the ESP8266

Some ESP8266 apparently boot with the serial port not set to 115200 baud. See
[https://docs.espressif.com/projects/esptool/en/latest/esp8266/advanced-topics/serial-protocol.html#initial-synchronisation](here)
for details.

Therefore we cannot try to find the text "waiting for download\r\n" and must
just wait for the serial port read to time out.

The ESP8266's ROM loader has a two byte status in each response packet. In
contrast, the other chips' ROM loaders use 4 bytes.

My ESP8266's magic number is 0xFFF0C101.

### Detecting the ESP32

My ESP32's magic number is 0x00F01D83.

### Detecting the ESP32-S2

My ESP32-S2's magic number is 0x000007C6.

### Detecting the ESP32-S3

My ESP32-S3's magic number is 0x00000009.

The ESP32-S3 technical reference manual doesn't list SPI register addresses,
but they do share a base address with the ESP32-C3 and using the same regs
seem to work to read the flash id.

### Detecting the ESP32-C3

My ESP32-C3's magic number is 0x1B31506F.

## Changing the baud rate

### Changing the ESP32-C3's baud rate

Setting the baud rate to something smaller than 115200 causes timeouts. I
should attach my logic analyzer to debug.

## ESP8266 ROM loader's erase bug

The ROM loader on the ESP8266 has a
[bug](https://docs.espressif.com/projects/esptool/en/latest/esp8266/advanced-topics/serial-protocol.html#erase-size-bug)
with its erase size computation. The bug can likely be deduced by examining
the code to work around it.


## `binrw` notes

Attribute order for reading (`binread`).

### Reading unit structs
1. `import`, `import_tuple`
2. Byte order determination
3. `magic`
4. `pre_assert`

### Reading structs
1. `import`, `import_tuple`
2. Byte order determination
3. `magic`
4. `pre_assert`
5. Read each field
6. Calls `BinRead::after_parse()`
7. `assert`

Reading a field
1. If `temp` and either `default` or `ignore`, then no other attributes are consulted and the field is not read
2. `offset`
3. `import`, `import_tuple`
4. Byte order determination
5. `magic`
6. `restore_position`'s position is computed here
7. `if` (if the condition is not satisfied skip to the `assert` step)
8. `seek_before`
9. `pad_before`
10. `align_before`
11. `pad_size_to`'s position is computed here
12. `default`, `ignore` (which acts like `default`), or `calc` actions are taken; otherwise the field is read via `parse_with` or normally, passing `args`, `args_raw`, or `count`
13. If none of `default`, `ignore`, and `calc`, then `try` or `err_context`
14. `map` or `try_map`
15. `postprocess_now` or `deref_now` cause `BinRead::after_parse()` to be called
16. `pad_size_to`
17. `pad_after`
18. `align_after`
19. `assert`
20. `restore_position`

## Reading unit enums
1. `import`, `import_tuple`
2. Byte order determination
3. `magic`
4. `pre_assert`
5. `repr` or fields' `magic` and `pre_assert`

## Reading data enums
1. `import`, `import_tuple`
2. Byte order determination
3. `magic`
4. `pre_assert`
5. `return_all_errors`, `return_unexpected_error`
6. If a unit variant, read according to [Reading unit structs](#reading-unit-structs), otherwise read according to [Reading structs](#reading-structs)
