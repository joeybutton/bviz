load("@rules_proto//proto:defs.bzl", "proto_descriptor_set", "proto_library")

package(default_visibility = ["//visibility:public"])

proto_library(
    name = "bazel_query_proto",
    srcs = glob(["**/*.proto"]),
)

proto_descriptor_set(
    name = "bazel_query_proto_descriptor_set",
    deps = [":bazel_query_proto"],
)
