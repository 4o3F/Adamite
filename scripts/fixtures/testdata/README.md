# ntfsinfo samples

These samples were captured without mounting from the committed `basic-4k.img.zst` and `frag-512.img.zst` images using ntfsinfo v2022.10.3 (libntfs-3g), `LC_ALL=C.UTF-8`, `TZ=UTC`, and `ntfsinfo -v -i <record> <decompressed-image>`.

| Sample       | Image    | Record |
| ------------ | -------- | -----: |
| mft          | basic-4k |      0 |
| ads          | basic-4k |    271 |
| sparse       | basic-4k |    316 |
| compressed   | basic-4k |    318 |
| index        | basic-4k |     67 |
| extend       | basic-4k |     11 |
| extend-child | basic-4k |     24 |
| reserved     | basic-4k |     12 |
| secure       | basic-4k |      9 |
| continuation | frag-512 |  10744 |

The index sample omits verbose INDX entry contents after the mapping pairs; its header, metadata, stream sizes and mapping pairs are unchanged. The continuation sample retains all 600 DATA runs, including the tool's `RL_NOT_MAPPED` placeholders, which are not physical runs. Reserved record 12 is in use and has no FILE_NAME.

The secure sample includes the separately captured stderr. The tool prints all stream headers, mapping pairs and the end-of-inode marker before reporting a failure to decode the SDS security-descriptor payload. Generation retains these header measurements and prints this diagnostic; other errors fail generation.
