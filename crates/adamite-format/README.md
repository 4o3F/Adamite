# adamite-format

`adamite-format` provides display types shared by the [CLI](../adamite-cli/README.md) and [terminal browser](../adamite-tui/README.md). All public types are exported at the crate root; formatting performs no source I/O.

| Type                                                  | Behavior                                                                                                                                                                                                               |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ByteSize { value: u128, raw_bytes: bool }`           | Displays a raw integer when requested; otherwise uses bytes or binary units from KiB through EiB with one rounded decimal place.                                                                                       |
| `Escaped(&str)`                                       | Preserves printable text and replaces control characters with `\u{…}` escapes for terminal display.                                                                                                                    |
| `ExtensionLabel(&str)`                                | Escapes extension text and displays `(none)` for an empty extension.                                                                                                                                                   |
| `ScanTime(Option<u64>)`                               | Converts FILETIME ticks to RFC 3339 UTC at seconds precision. `rfc3339()` returns `None` for absent or unrepresentable values; `Display` uses `unknown`.                                                               |
| `VolumeSerial(u64)`                                   | Displays sixteen lowercase hexadecimal digits, padded with zeros.                                                                                                                                                      |
| `InfoFields { output: &ScanOutput, raw_bytes: bool }` | `rows()` returns ordered `(label, value)` strings: serial, optional label, sector/cluster/volume sizes, optional free space, node/file totals, logical/allocated totals and nonzero scan counters in core field order. |

```rust
use adamite_format::{ByteSize, Escaped, ScanTime};

assert_eq!(ByteSize { value: 1536, raw_bytes: false }.to_string(), "1.5 KiB");
assert_eq!(Escaped("name\n").to_string(), "name\\u{a}");
assert_eq!(ScanTime(Some(0)).to_string(), "1601-01-01T00:00:00Z");
```
