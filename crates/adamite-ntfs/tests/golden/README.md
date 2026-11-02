# Boot-sector vectors

`windows-2000-boot.bin` transcribes all 512 bytes of the Windows 2000 formatted volume in [LSoft's published boot-sector dump](https://ntfs.com/ntfs-partition-boot-sector.htm). The expected geometry is 512-byte sectors, 8 sectors per cluster, 1024-byte file records, 4096-byte index records, 0x7FF54A sectors, MFT LCN 4, MFTMirr LCN 0x7FF54 and serial 0x1C741BC9741BA514.

`mkntfs-boot.bin` contains the first 512 bytes of a 16 MiB sparse regular file formatted with ntfs-3g 2022.10.3 using `mkntfs -F -Q -q -T -s 512 -c 4096 -L ADAMITE_GOLDEN <image>`. Formatting succeeded without mounting or elevated privileges. The geometry is 512-byte sectors, 8 sectors per cluster, 1024-byte file records, 4096-byte index records, 32767 sectors, MFT LCN 4, MFTMirr LCN 2047 and serial 3816218020381368311. Regeneration changes the serial; update the expected value with the binary.
