// Shared primary/secondary host baseline; clobbers x9 only. DAIF must be masked.
    mov x9, #{host_hcr}
    msr hcr_el2, x9
    mov x9, #{host_cptr}
    msr cptr_el2, x9
    mov x9, #{host_hstr}
    msr hstr_el2, x9
    mov x9, #{host_vttbr}
    msr vttbr_el2, x9
    mov x9, #{host_cntvoff}
    msr cntvoff_el2, x9
    mov x9, #{host_cnthctl}
    msr cnthctl_el2, x9
    mov x9, #{host_timer}
    msr cnthp_ctl_el2, x9
    msr cntp_ctl_el0, x9
    msr cntv_ctl_el0, x9
    isb
