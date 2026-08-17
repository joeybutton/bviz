load("@package_metadata//licenses/rules:license.bzl", "license")
load("@package_metadata//rules:package_metadata.bzl", "package_metadata")
load("@rules_proto//proto:defs.bzl", "proto_descriptor_set", "proto_library")
load("@rules_rust_prost//:defs.bzl", "rust_prost_library")

package(
    default_package_metadata = [":package_metadata"],
    default_visibility = ["//visibility:public"],
)

package_metadata(
    name = "package_metadata",
    attributes = [":license"],
    purl = "pkg:github/bazelbuild/bazel@43f23b4349195333b97bef77cc62c2ea4ffd8e91",
    visibility = ["//visibility:private"],
)

license(
    name = "license",
    kind = "@package_metadata//licenses/spdx:Apache-2.0",
    text = "LICENSE",
    visibility = ["//visibility:private"],
)

proto_library(
    name = "bazel_query_proto",
    srcs = glob(["**/*.proto"]),
)

rust_prost_library(
    name = "bazel_query_rust_proto",
    proto = ":bazel_query_proto",
)

proto_descriptor_set(
    name = "bazel_query_proto_descriptor_set",
    deps = [":bazel_query_proto"],
)
