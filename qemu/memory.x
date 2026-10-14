/* QEMU mps2-an385: 4 MiB SSRAM1 at 0 for code, 4 MiB SSRAM2/3 at 0x20000000.
   The flash image under test is loaded at FLASH_BASE (0x20300000), clear of
   the 1 MiB the program uses for its stack and data. */
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 1M
  RAM   : ORIGIN = 0x20000000, LENGTH = 1M
}
