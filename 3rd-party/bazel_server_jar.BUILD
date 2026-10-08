load("@package_metadata//licenses/rules:license.bzl", "license")
load("@package_metadata//rules:package_metadata.bzl", "package_metadata")
load("@rules_java//java:java_import.bzl", "java_import")

package(default_package_metadata = [":package_metadata"])

package_metadata(
    name = "package_metadata",
    attributes = [":license"],
    purl = "pkg:github/bazelbuild/bazel@9.2.0",
)

# Covers Bazel's own work in the jar, whose text the source tree of the same
# release carries. The libraries the jar bundles (Guava, protobuf, gRPC and
# others) are under their own terms and not declared here: the jar only runs
# bviz's tests and never reaches anything bviz distributes.
license(
    name = "license",
    kind = "@package_metadata//licenses/spdx:Apache-2.0",
    text = "@bazel_query_proto//:LICENSE",
)

java_import(
    name = "server",
    testonly = True,
    jars = ["A-server.jar"],
    visibility = [
        "@//bazel_flags/oracle:__pkg__",
        "@//graph/layout/oracle:__pkg__",
    ],
)
