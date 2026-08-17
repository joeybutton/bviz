load("@package_metadata//licenses/rules:license.bzl", "license")
load("@package_metadata//rules:package_metadata.bzl", "package_metadata")

package(default_package_metadata = [":package_metadata"])

package_metadata(
    name = "package_metadata",
    attributes = [":license"],
    purl = "pkg:github/jetbrains/jetbrainsmono@v2.304",
)

license(
    name = "license",
    kind = "@package_metadata//licenses/spdx:OFL-1.1-no-RFN",
    text = "OFL.txt",
)

filegroup(
    name = "fonts",
    srcs = [
        "fonts/ttf/JetBrainsMono-Bold.ttf",
        "fonts/ttf/JetBrainsMono-BoldItalic.ttf",
        "fonts/ttf/JetBrainsMono-Italic.ttf",
        "fonts/ttf/JetBrainsMono-Regular.ttf",
    ],
    visibility = ["//visibility:public"],
)
