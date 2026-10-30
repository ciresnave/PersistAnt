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

The latest release line receives security fixes. An older line receives a backport when the backport is small. When it is not, we do not attempt it: the advisory says that line is not fixed and recommends upgrading. No support period is promised.
