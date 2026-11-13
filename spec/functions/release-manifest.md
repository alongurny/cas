# Function: release-manifest

Input: a `release`. Output: a `manifest`.

The output lists every blob the release brings: the release itself, its
tag, the changelog of each step from an earlier release, and each blob it
reaches through its version that is not in the same place under the
version of one of the releases it follows. Files, and anything reached only
through an earlier release, are left out.

"In the same place" is decided as in [manifest](manifest.md), with the
releases it follows in the place of a commit's parents: the version is
looked at against their versions and the tag against their tags, and a
changelog is always the release's own.

So every blob a release history reaches that is not a file is listed by the
manifest of a release in it, and a proof of each of those manifests says
the whole history is valid. A first release lists its whole version.
