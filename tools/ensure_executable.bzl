"""Materialize a prebuilt tool as an executable file.

rules_multitool delivers tools whose upstream release is a bare gzipped binary
(a `.gz`, not a tar/zip) by extracting the single file, which carries no Unix
mode bits, so the result is mode 0644. Its `tool` rule then only symlinks that
file, and a symlink cannot grant an executable bit its target lacks, so running
the tool fails with "non-executable". Copying the payload into a fresh output
and setting the bit produces a genuinely runnable binary.
"""

def _ensure_executable_impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.out or ctx.label.name)
    ctx.actions.run_shell(
        inputs = [ctx.executable.src],
        outputs = [out],
        arguments = [ctx.executable.src.path, out.path],
        command = 'cp -fL -- "$1" "$2" && chmod +x -- "$2"',
        mnemonic = "EnsureExecutable",
        execution_requirements = {"no-remote-exec": "1"},
    )
    return [DefaultInfo(
        executable = out,
        runfiles = ctx.runfiles(files = [out]),
    )]

ensure_executable = rule(
    implementation = _ensure_executable_impl,
    executable = True,
    attrs = {
        "src": attr.label(
            mandatory = True,
            executable = True,
            cfg = "target",
            doc = "Prebuilt binary to materialize as an executable.",
        ),
        "out": attr.string(doc = "Output file name; defaults to the target name."),
    },
)
