"""Collects the licence terms of what a binary links and writes its notices."""

load("@package_metadata//providers:package_metadata_info.bzl", "PackageMetadataInfo")
load("@rules_rs_rules_rust//rust:rust_common.bzl", "CrateInfo")

# The attributes whose targets end up in a binary: linked code, and the sources
# and data compiled or embedded into it.
_LINKED_ATTRS = [
    "actual",
    "aliases",
    "allocator_libraries",
    "allocator_library",
    "compile_data",
    "data",
    "deps",
    "dynamic_deps",
    "global_allocator_library",
    "implementation_deps",
    "link_deps",
    "proto",
    "srcs",
]

# The attributes through which a target puts files of its own into what links
# it, rather than naming another target that contributes them.
_FILE_ATTRS = [
    "compile_data",
    "data",
    "hdrs",
    "interface_library",
    "objects",
    "pic_objects",
    "shared_library",
    "srcs",
    "static_library",
    "textual_hdrs",
]

# rules_rs writes a crate's Cargo manifest fields to this file at the crate root.
_CARGO_ENV_FILE = "cargo_toml_env_vars.env"

# A licence file is named for one of these words, on its own or followed by a
# separator and a variant such as -APACHE, .LESSER or _WITH_LLVM-exception.
_LICENCE_FILE_STEMS = ["copying", "copyright", "licence", "license", "notice", "unlicense"]

_LICENCE_FILE_SEPARATORS = ["-", "_", "."]

# Names above that end in one of these belong to a tool that handles licences,
# such as license_parser.py, rather than to a licence.
_SOURCE_FILE_EXTENSIONS = [
    "bat",
    "bzl",
    "c",
    "cc",
    "cfg",
    "cmake",
    "cpp",
    "cs",
    "css",
    "go",
    "h",
    "hpp",
    "in",
    "ini",
    "java",
    "js",
    "json",
    "jsx",
    "kt",
    "lock",
    "mjs",
    "pl",
    "ps1",
    "py",
    "rb",
    "rs",
    "sh",
    "toml",
    "ts",
    "tsx",
    "xml",
    "yaml",
    "yml",
]

# The FreeType License's own file name, which the FreeType sources vendored by
# its -sys crate refer to.
_LICENCE_FILE_NAMES = ["ftl.txt"]

# Licence files below these directories belong to tests, fixtures or tooling
# rather than to anything a crate compiles.
_UNLINKED_DIRS = ["benches", "examples", "fuzz", "scripts", "test", "testdata", "tests"]

# Rule kinds whose own sources run the build rather than reaching a binary. What
# such a rule copies or generates does reach one, so the aspect still follows it.
_BUILD_TIME_KINDS = ["cargo_build_script"]

_NoticesInfo = provider(
    doc = "The third-party work a target contributes to the binaries linking it.",
    fields = {
        "crates": "depset of structs: a crate's Cargo manifest fields, its package metadata and licence files",
        "files": "depset of the Files every struct above reads",
        "metadata": "depset of package_metadata JSON Files declared on the targets",
        "undeclared": "depset of Labels of external targets contributing files to the binary that declare no package metadata",
    },
)

_EMPTY = _NoticesInfo(
    crates = depset(),
    files = depset(),
    metadata = depset(),
    undeclared = depset(),
)

def _has_licence_name(basename):
    name = basename.lower()
    if name in _LICENCE_FILE_NAMES:
        return True
    if "." in name and name.rpartition(".")[2] in _SOURCE_FILE_EXTENSIONS:
        return False
    for stem in _LICENCE_FILE_STEMS:
        if name == stem:
            return True
        if name.startswith(stem) and name[len(stem)] in _LICENCE_FILE_SEPARATORS:
            return True
    return False

def _is_licence_file(file, crate_root):
    if not file.path.startswith(crate_root + "/"):
        return False
    if not _has_licence_name(file.basename):
        return False
    subdirs = file.path[len(crate_root) + 1:].split("/")[:-1]
    return not [d for d in subdirs if d.lower() in _UNLINKED_DIRS]

def _declared_metadata(rule_attr):
    targets = getattr(rule_attr, "package_metadata", None) or []
    return [t[PackageMetadataInfo] for t in targets if PackageMetadataInfo in t]

def _cargo_env_file(rule_files):
    for f in getattr(rule_files, "rustc_env_files", []):
        if f.basename == _CARGO_ENV_FILE:
            return f
    return None

def _linked_targets(value):
    """The targets an attribute of `_LINKED_ATTRS` names, whatever its shape."""
    if type(value) == "list":
        return value
    if type(value) == "dict":
        return value.keys()
    return [value]

def _contributes_sources(rule_files):
    """Whether the target puts files a repository ships into what links it.

    Only source files count: a generated one carries what the rule writing it
    put there, and that rule lives in a package of its own.
    """
    for attr in _FILE_ATTRS:
        for f in getattr(rule_files, attr, []):
            if f.is_source:
                return True
    return False

def _notices_aspect_impl(target, ctx):
    # A proc-macro runs the compiler rather than linking into what it expands.
    if CrateInfo in target and target[CrateInfo].type == "proc-macro":
        return [_EMPTY]

    children = []
    for attr in _LINKED_ATTRS:
        value = getattr(ctx.rule.attr, attr, None)
        if value == None:
            continue
        for dep in _linked_targets(value):
            # A rule is free to give one of these names to an attribute that
            # holds something other than a label.
            if type(dep) == "Target" and _NoticesInfo in dep:
                children.append(dep[_NoticesInfo])

    declared = _declared_metadata(ctx.rule.attr)
    env_file = _cargo_env_file(ctx.rule.files) if CrateInfo in target else None
    external = target.label.repo_name != ""

    crates = []
    files = [m.files for m in declared]
    metadata = []
    undeclared = []
    if env_file and external:
        if len(declared) != 1:
            fail("{} must declare exactly one package_metadata, not {}".format(target.label, len(declared)))
        licence_files = [
            f
            for f in getattr(ctx.rule.files, "compile_data", [])
            if _is_licence_file(f, env_file.dirname)
        ]
        crates.append(struct(
            env = env_file.path,
            metadata = declared[0].metadata.path,
            licence_files = tuple([f.path for f in licence_files]),
        ))
        files.append(depset([env_file] + licence_files))
    else:
        metadata = [m.metadata for m in declared]
        if (external and not declared and
            ctx.rule.kind not in _BUILD_TIME_KINDS and
            _contributes_sources(ctx.rule.files)):
            undeclared.append(target.label)

    return [_NoticesInfo(
        crates = depset(crates, transitive = [c.crates for c in children]),
        files = depset(transitive = files + [c.files for c in children]),
        metadata = depset(metadata, transitive = [c.metadata for c in children]),
        undeclared = depset(undeclared, transitive = [c.undeclared for c in children]),
    )]

_notices_aspect = aspect(
    implementation = _notices_aspect_impl,
    attr_aspects = _LINKED_ATTRS,
)

def _repository_of(metadata):
    """The repository holding the licence text a package_metadata declares."""
    texts = [f for f in metadata.files.to_list() if f.is_source]
    repos = {f.owner.repo_name: None for f in texts if f.owner.repo_name}
    if len(repos) != 1:
        fail("{} must declare its licence with a text from one external repository".format(metadata.metadata.path))
    return repos.keys()[0]

def _third_party_notices_impl(ctx):
    info = ctx.attr.binary[_NoticesInfo]

    repository_metadata = {
        _repository_of(t[PackageMetadataInfo]): t[PackageMetadataInfo]
        for t in ctx.attr.repository_package_metadata
    }
    used_repository_metadata = {}
    unresolved = []
    for label in info.undeclared.to_list():
        m = repository_metadata.get(label.repo_name)
        if m:
            used_repository_metadata[label.repo_name] = m
        else:
            unresolved.append(str(label))
    if unresolved:
        fail(("{} is built from external targets whose repositories declare no licence; " +
              "add a package_metadata for each to repository_package_metadata:\n  {}").format(
            ctx.attr.binary.label,
            "\n  ".join(sorted(unresolved)),
        ))

    # package_metadata targets are analysed without a configuration of their
    # own, so a declaration is identified by its path below the output root.
    own = ctx.attr.own_package_metadata[PackageMetadataInfo].metadata.short_path
    extra = [t[PackageMetadataInfo] for t in ctx.attr.toolchain_package_metadata] + used_repository_metadata.values()
    metadata = {
        m.short_path: m
        for m in info.metadata.to_list() + [m.metadata for m in extra]
        if m.short_path != own
    }

    spdx_texts = ctx.files._spdx_texts
    if not spdx_texts:
        fail("{} holds no SPDX License List texts".format(ctx.attr._spdx_texts.label))

    manifest = ctx.actions.declare_file(ctx.label.name + ".manifest.json")
    ctx.actions.write(manifest, json.encode(struct(
        crates = info.crates.to_list(),
        package_metadata = [metadata[k].path for k in sorted(metadata)],
        denied_licences = ctx.attr.denied_licences,
        preferred_licences = ctx.attr.preferred_licences,
        spdx_texts = [f.path for f in spdx_texts],
    )))

    output = ctx.actions.declare_file(ctx.label.name + ".txt")
    args = ctx.actions.args()
    args.add("--manifest", manifest)
    args.add("--components", ctx.file.components)
    args.add("--output", output)
    ctx.actions.run(
        executable = ctx.executable._generator,
        arguments = [args],
        inputs = depset(
            [manifest, ctx.file.components] + spdx_texts,
            transitive = [info.files] + [m.files for m in extra],
        ),
        outputs = [output],
        mnemonic = "ThirdPartyNotices",
        progress_message = "Writing third-party notices for %{label}",
    )
    return [
        DefaultInfo(files = depset([output])),
        OutputGroupInfo(manifest = depset([manifest])),
    ]

third_party_notices = rule(
    implementation = _third_party_notices_impl,
    doc = """Writes the licence texts of the third-party work linked or embedded into a binary.

A component is taken under one alternative of its licence expression that names
no denied licence, and the notices carry that alternative's texts: the one taken
wholly under preferred licences whose least preferred licence is listed
earliest, ties going to fewer terms and then to the first the expression
states. The build fails when a component declares no licence, when every
alternative it offers names a denied licence, when a text it ships carries a
denied licence its terms do not cover, or when a licence it is taken under
leaves neither a text nor a copyright holder to name.""",
    attrs = {
        "binary": attr.label(
            mandatory = True,
            aspects = [_notices_aspect],
        ),
        "components": attr.label(
            allow_single_file = True,
            default = "//tools/notices:components.json",
            doc = "The in-repo table of licence facts a component's own metadata lacks.",
        ),
        "denied_licences": attr.string_list(
            doc = "SPDX identifier prefixes no component may be taken under; one whose licence expression leaves no alternative fails the build.",
        ),
        "own_package_metadata": attr.label(
            mandatory = True,
            providers = [PackageMetadataInfo],
            doc = "The binary's own package_metadata, left out of its third-party notices.",
        ),
        "preferred_licences": attr.string_list(
            doc = "SPDX identifiers a component is taken under where its licence expression offers them, earliest first.",
        ),
        "repository_package_metadata": attr.label_list(
            providers = [PackageMetadataInfo],
            doc = "Licences of external C/C++ libraries that declare none; each covers the repository its licence text comes from.",
        ),
        "toolchain_package_metadata": attr.label_list(
            providers = [PackageMetadataInfo],
            doc = "Licences of work a toolchain links into every binary, which no dependency edge reaches.",
        ),
        "_generator": attr.label(
            default = "//tools/notices",
            executable = True,
            cfg = "exec",
        ),
        "_spdx_texts": attr.label(
            default = "@spdx_license_list//:texts",
        ),
    },
)
