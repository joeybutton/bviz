load("@package_metadata//licenses/rules:license.bzl", "license")
load("@package_metadata//rules:package_metadata.bzl", "package_metadata")

package(default_package_metadata = [":package_metadata"])

package_metadata(
    name = "package_metadata",
    attributes = [":license"],
    purl = "pkg:github/spdx/license-list-data@v3.29.0",
)

# The archive carries no licence file of its own, but its own terms are one of
# the texts it publishes.
license(
    name = "license",
    kind = "@package_metadata//licenses/spdx:CC-BY-3.0",
    text = "text/CC-BY-3.0.txt",
)

filegroup(
    name = "texts",
    srcs = glob(["text/*.txt"]),
    visibility = ["//visibility:public"],
)
