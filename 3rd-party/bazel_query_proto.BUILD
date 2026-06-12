load("@rules_proto//proto:defs.bzl", "proto_descriptor_set", "proto_library")
load("@rules_rust_prost//:defs.bzl", "rust_prost_library")

package(default_visibility = ["//visibility:public"])

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
