# Remote daemon publication is verified

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-mux::remote_install` owns publication of the exact versioned daemon path
on a remote host. A Unix installer validates one unique same-directory candidate
before it atomically replaces the installed path.

## Authority and invariants

- The local artifact provider owns target selection and checksum verification.
- The remote installer owns upload, candidate validation, publication, and final
  installed-path validation.
- A Unix candidate uses a unique path beside the installed path.
- The Unix candidate has mode `0700` before Bootty executes it.
- Candidate validation requires the exact daemon protocol and package version.
- Unix publication uses one same-filesystem atomic rename.
- The installed path is validated again after publication.
- A running old Unix daemon can keep its old inode. New commands resolve the
  replacement path.
- Concurrent installers can publish compatible candidates. Success still
  requires the final installed path to pass the exact validation.
- Windows keeps its current first-writer publication. A versioned Windows path
  avoids normal release collisions and cannot use the Unix running-inode rule.

## Failure and recovery

An upload, permission, or candidate-validation failure occurs before publication.
It leaves the prior installed path unchanged. Cleanup removes only the unique
candidate. A later installation attempt can retry.

A publication failure cleans the candidate and accepts a concurrent winner only
after the installed path passes exact validation. A failed final validation
reports installation failure after replacement. Bootty does not claim that the
prior path remains active after atomic publication. The installer never accepts
a candidate or competing winner without an exact daemon protocol and
package-version response.

## Rejected alternatives

- A hard link cannot replace an incompatible existing installed path.
- Deleting the installed path before publication creates an unavailable window.
- A stable launcher, remote registry, background updater, or daemon stop command
  adds lifecycle state that this replacement does not need.
- A remote lock protocol is unnecessary because final-path validation handles
  compatible concurrent publication.
- Remote protocol changes and asset renames do not improve publication safety.

## Compatibility

The versioned installed path, remote command grammar, target detection, SSH
quoting, local checksum validation, and Windows behavior do not change.
