# Architecture Log
## 2026-10-24 → 2026-10-24 — Runtime directory suffix is the config directory owner's uid
KIND:          runnable
FROM → TO:     No runtime directory existed → `<base>/estafette-<uid>` where uid is the owner of the resolved config directory, read through MetadataExt, since libc::getuid needs unsafe code, which the workspace forbids
WHY:           spec:516 puts the user id in the runtime directory name and leaves how it is read to the implementer (spec:450 call); the config directory is created by the same user before the runtime directory, so its owner is that user without unsafe code
INVALIDATES:   A config directory owned by another user yields that user's runtime directory; a deployment that pre-creates the config directory as root must chown it
               U30 (bridge) assumes the same `estafette-<uid>` suffix and is checked against this at integration
EVIDENCE:      core/src/dirs.rs at e935909; spec review 112404 with its correction; quality review 112512 Ready Yes

## 2026-10-24 → 2026-10-24 — Card public mapping serializes with its keys in sorted order
KIND:          computed
FROM → TO:     No converter existed → the card's `public` mapping is rendered to compact JSON with keys in sorted byte order, whatever order the YAML source lists them; the bytes then travel and are stored verbatim (spec:591)
WHY:           spec:591 fixes compact JSON and leaves key order unstated; one fixed order makes the serialized bytes a function of the mapping's content alone, so two renderings of one card agree (spec:450 call of U10, lead ruling at merge)
INVALIDATES:   A card whose public keys are written in a deliberate order sees that order lost on the wire; nothing in the spec reads order
EVIDENCE:      core/src/card.rs at fc6ebf3; spec review 112930 PASS; quality review 113032 Ready Yes
