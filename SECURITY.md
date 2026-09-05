# Security

Report vulnerabilities privately to yair.etziony@gmail.com. Say what you
found, how to reproduce it, and which version; you will get an
acknowledgement within a week. There is no bug bounty.

Sykli runs the commands a repository declares, on the machine it is invoked
on, with that user's privileges. It does not sandbox them; a contract is as
trusted as the repository it lives in. Receipts are evidence of what ran,
bound to the tree and contract hash, and `sykli verify` detects a receipt
that no longer matches its tree; it does not detect a malicious task.

Release tarballs are checked against `SHA256SUMS` by `install.sh`, and the
Homebrew formula pins the same checksums.
