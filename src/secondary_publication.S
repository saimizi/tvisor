// CPU 0 publication primitives. Call only with validated identity addresses.
.section .text.secondary_publication, "ax"
.balign 4
.global __secondary_clean_line
.type __secondary_clean_line, %function
__secondary_clean_line:
    dc cvac, x0
    ret
.size __secondary_clean_line, . - __secondary_clean_line
.global __secondary_publication_barrier
.type __secondary_publication_barrier, %function
__secondary_publication_barrier:
    dsb sy
    ret
.size __secondary_publication_barrier, . - __secondary_publication_barrier
.global __secondary_read_cell
.type __secondary_read_cell, %function
__secondary_read_cell:
    ldr x0, [x0]
    ret
.size __secondary_read_cell, . - __secondary_read_cell
.global __secondary_store_cell
.type __secondary_store_cell, %function
__secondary_store_cell:
    isb
    mrs x2, cntpct_el0
    str x1, [x0]
    mov x0, x2
    ret
.size __secondary_store_cell, . - __secondary_store_cell
.global __secondary_send_event
.type __secondary_send_event, %function
__secondary_send_event:
    sev
    ret
.size __secondary_send_event, . - __secondary_send_event
