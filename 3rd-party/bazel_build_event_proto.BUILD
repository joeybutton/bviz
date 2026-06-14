load("@rules_proto//proto:defs.bzl", "proto_descriptor_set", "proto_library")
load("@rules_rust_prost//:defs.bzl", "rust_prost_library")

package(default_visibility = ["//visibility:public"])

proto_library(
    name = "bazel_build_event_proto",
    srcs = glob(["**/*.proto"]),
    deps = [
        "@com_google_protobuf//:any_proto",
        "@com_google_protobuf//:descriptor_proto",
        "@com_google_protobuf//:duration_proto",
        "@com_google_protobuf//:timestamp_proto",
    ],
)

proto_descriptor_set(
    name = "bazel_build_event_proto_descriptor_set",
    deps = [":bazel_build_event_proto"],
)

rust_prost_library(
    name = "bazel_build_event_rust_proto",
    proto = ":bazel_build_event_proto",
)
