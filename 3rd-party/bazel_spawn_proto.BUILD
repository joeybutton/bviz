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
    purl = "pkg:github/bazelbuild/bazel@9.2.0",
    visibility = ["//visibility:private"],
)

license(
    name = "license",
    kind = "@package_metadata//licenses/spdx:Apache-2.0",
    text = "LICENSE",
    visibility = ["//visibility:private"],
)

proto_library(
    name = "bazel_spawn_proto",
    srcs = [
        "src/main/protobuf/execution_graph.proto",
        "src/main/protobuf/spawn.proto",
    ],
    deps = [
        "@com_google_protobuf//:duration_proto",
        "@com_google_protobuf//:timestamp_proto",
    ],
)

proto_descriptor_set(
    name = "bazel_spawn_proto_descriptor_set",
    deps = [":bazel_spawn_proto"],
)

rust_prost_library(
    name = "bazel_spawn_rust_proto",
    proto = ":bazel_spawn_proto",
)
