# ntfsinfo samples

These samples were captured without mounting from the committed `basic-4k.img.zst` and `frag-512.img.zst` images using ntfsinfo v2022.10.3 (libntfs-3g), `LC_ALL=C.UTF-8`, `TZ=UTC`, and `ntfsinfo -v -i <record> <decompressed-image>`.

| Sample           | Image    | Record |
| ---------------- | -------- | -----: |
| mft              | basic-4k |      0 |
| ads              | basic-4k |    271 |
| sparse           | basic-4k |    316 |
| compressed       | basic-4k |    318 |
| index            | basic-4k |     67 |
| extend           | basic-4k |     11 |
| extend-child     | basic-4k |     24 |
| reserved         | basic-4k |     12 |
| secure           | basic-4k |      9 |
| continuation     | frag-512 |  10744 |
| fragmented-index | frag-512 |     64 |

The index sample omits verbose INDX entry contents after the mapping pairs; its header, metadata, stream sizes and mapping pairs are unchanged. The continuation sample retains all 600 DATA runs, including the tool's `RL_NOT_MAPPED` placeholders, which are not physical runs. Reserved record 12 is in use and has no FILE_NAME.

The secure and fragmented-index samples include separately captured stderr files. The tool prints complete headers and mapping pairs before its payload decoder reports errors. The fragmented index has three segments, 513 runs and three diagnostic pairs; only its first segment reports stream sizes. Generation accepts paired payload diagnostics only when the inode matches, the affected headers and mapping pairs validate, the end-of-inode marker is present and the exit code is zero. It prints one summary per affected inode. Unknown stderr and incomplete measurements fail generation.
