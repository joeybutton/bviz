"""The SOURCE file of a release archive, naming the commit it was built from."""

# The commit is copied out of the stable status into a file of its own, so a
# change to any other status key re-runs only that copy, whose unchanged output
# leaves the SOURCE text and the archives cached.
_COPY_COMMIT = """
while read -r key value; do
    if [ "$key" = STABLE_SOURCE_COMMIT ]; then
        printf '%s\\n' "$value"
    fi
done < "$1" > "$2"
"""

_WRITE_SOURCE = """
read -r commit < "$1"
if [ -n "$commit" ]; then
    printf 'Built from %s at commit %s.\\n' "$3" "$commit"
else
    printf 'Built from %s; no commit was recorded for this build.\\n' "$3"
fi > "$2"
"""

def _source_file_impl(ctx):
    commit = ctx.actions.declare_file(ctx.label.name + ".commit")
    ctx.actions.run_shell(
        inputs = [ctx.info_file],
        outputs = [commit],
        arguments = [ctx.info_file.path, commit.path],
        command = _COPY_COMMIT,
        mnemonic = "SourceCommit",
    )
    out = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.run_shell(
        inputs = [commit],
        outputs = [out],
        arguments = [commit.path, out.path, ctx.attr.repository],
        command = _WRITE_SOURCE,
        mnemonic = "SourceText",
    )
    return [DefaultInfo(files = depset([out]))]

source_file = rule(
    implementation = _source_file_impl,
    attrs = {
        "repository": attr.string(
            mandatory = True,
            doc = "URL of the repository the commit is in.",
        ),
    },
    doc = "Writes the text naming the commit the build's stable status records as " +
          "STABLE_SOURCE_COMMIT, or saying that none was recorded.",
)
