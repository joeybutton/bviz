"""A repository of individually downloaded files."""

def _http_files_impl(rctx):
    downloads = [
        rctx.download(rctx.attr.url_prefix + path, path, sha256 = sha256, block = False)
        for path, sha256 in rctx.attr.files.items()
    ]
    for download in downloads:
        download.wait()
    rctx.symlink(rctx.attr.build_file, "BUILD.bazel")
    return rctx.repo_metadata(reproducible = True)

http_files = repository_rule(
    implementation = _http_files_impl,
    doc = "Downloads each file to its own path within the repository.",
    attrs = {
        "build_file": attr.label(mandatory = True, allow_single_file = True),
        "files": attr.string_dict(
            doc = "SHA-256 of each file, keyed by its path in the repository and below `url_prefix`.",
            mandatory = True,
        ),
        "url_prefix": attr.string(mandatory = True),
    },
)
