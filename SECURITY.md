# Security policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately through GitHub's private vulnerability reporting for
this repository: <https://github.com/ciresnave/PersistAnt/security/advisories/new>
(the **Security** tab, then **Report a vulnerability**).

Please do not open a public issue or pull request for a suspected vulnerability.

Include the version (`persistant` x.y.z), what you did, what you expected, and what happened. You can
expect an acknowledgement; this is a small project, so please allow some days for a first reply.

## Scope

PersistAnt is a thin layer over [Apache OpenDAL](https://opendal.apache.org/). Reports about the layer's
own behaviour (for example an atomic replace that is not atomic, a capability check that accepts a backend
it should refuse, or a record read that accepts data of the wrong kind or schema) are in scope. Defects in
OpenDAL itself should be reported to that project; tell us as well if PersistAnt's use of it makes the
defect worse.

## Supported versions

Only the latest published release receives fixes while the crate is pre-1.0.
