load("@package_metadata//licenses/rules:license.bzl", "license")
load("@package_metadata//rules:package_metadata.bzl", "package_metadata")

package(default_package_metadata = [":package_metadata"])

package_metadata(
    name = "package_metadata",
    attributes = [":license"],
    purl = "pkg:github/rsms/inter@v4.1",
)

license(
    name = "license",
    kind = "@package_metadata//licenses/spdx:OFL-1.1-no-RFN",
    text = "LICENSE.txt",
)

filegroup(
    name = "fonts",
    srcs = [
        "extras/ttf/Inter-Bold.ttf",
        "extras/ttf/Inter-BoldItalic.ttf",
        "extras/ttf/Inter-Italic.ttf",
        "extras/ttf/Inter-Regular.ttf",
    ],
    visibility = ["//visibility:public"],
)
