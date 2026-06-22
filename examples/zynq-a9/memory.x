/*
Memory configuration for the QEMU `xilinx-zynq-a9` machine (Cortex-A9 MPCore).

DDR starts at 0x0; we link into it but stay clear of the low 1 MiB.
*/

MEMORY {
    DDR : ORIGIN = 0x00100000, LENGTH = 32M
}

REGION_ALIAS("VECTORS", DDR);
REGION_ALIAS("CODE", DDR);
REGION_ALIAS("DATA", DDR);
REGION_ALIAS("STACKS", DDR);

PROVIDE(_num_cores = 1);
