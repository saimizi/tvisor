# Vendored dependencies

## dtoolkit

`dtoolkit/` contains dtoolkit version 0.3.0 from
<https://github.com/google/dtoolkit>, upstream commit
`09b2b6a630dd7e492123609a815327483fd91dfd`.

It is distributed under `Apache-2.0 OR MIT`; the upstream license files and
source copyright notices are retained in the vendored directory. Tvisor uses
it under the MIT license.

Local changes:

- Permit property names longer than 31 characters. Real Raspberry Pi firmware
  DTBs contain `arm,cpu-registers-not-fw-configured`; the binary FDT format can
  represent this name even though it exceeds the specification's recommended
  limit. Character validation remains enabled.
