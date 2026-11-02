// Entry VA is an identity-mapped Normal descriptor in the retained table arena.
.section .text.tlb_sync, "ax"
.balign 4
.global __el2_scratch_sync
.type __el2_scratch_sync, %function
__el2_scratch_sync:
    dc cvac, x0
    // Complete both the descriptor store and its clean before invalidation.
    dsb ish
    tlbi alle2is
    dsb ish
    isb
    ret
.size __el2_scratch_sync, . - __el2_scratch_sync
