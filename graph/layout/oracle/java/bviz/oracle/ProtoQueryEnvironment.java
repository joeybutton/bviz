package bviz.oracle;

import com.google.common.collect.ImmutableList;
import com.google.common.collect.ImmutableMap;
import com.google.common.collect.ImmutableSet;
import com.google.devtools.build.lib.cmdline.BatchCallback;
import com.google.devtools.build.lib.cmdline.IgnoredSubdirectories;
import com.google.devtools.build.lib.cmdline.Label;
import com.google.devtools.build.lib.cmdline.LabelSyntaxException;
import com.google.devtools.build.lib.cmdline.PackageIdentifier;
import com.google.devtools.build.lib.cmdline.QueryExceptionMarkerInterface;
import com.google.devtools.build.lib.cmdline.RepositoryMapping;
import com.google.devtools.build.lib.cmdline.RepositoryName;
import com.google.devtools.build.lib.cmdline.ResolvedTargets;
import com.google.devtools.build.lib.cmdline.TargetParsingException;
import com.google.devtools.build.lib.cmdline.TargetPattern;
import com.google.devtools.build.lib.cmdline.TargetPatternResolver;
import com.google.devtools.build.lib.events.Event;
import com.google.devtools.build.lib.events.ExtendedEventHandler;
import com.google.devtools.build.lib.packages.LabelPrinter;
import com.google.devtools.build.lib.packages.Target;
import com.google.devtools.build.lib.query2.common.AbstractBlazeQueryEnvironment;
import com.google.devtools.build.lib.query2.engine.Callback;
import com.google.devtools.build.lib.query2.engine.KeyExtractor;
import com.google.devtools.build.lib.query2.engine.MinDepthUniquifier;
import com.google.devtools.build.lib.query2.engine.OutputFormatterCallback;
import com.google.devtools.build.lib.query2.engine.QueryEvalResult;
import com.google.devtools.build.lib.query2.engine.QueryException;
import com.google.devtools.build.lib.query2.engine.QueryExpression;
import com.google.devtools.build.lib.query2.engine.QueryExpressionContext;
import com.google.devtools.build.lib.query2.engine.QueryUtil.MinDepthUniquifierImpl;
import com.google.devtools.build.lib.query2.engine.QueryUtil.ThreadSafeMutableKeyExtractorBackedSetImpl;
import com.google.devtools.build.lib.query2.engine.QueryUtil.UniquifierImpl;
import com.google.devtools.build.lib.query2.engine.QueryVisibility;
import com.google.devtools.build.lib.query2.engine.ThreadSafeOutputFormatterCallback;
import com.google.devtools.build.lib.query2.engine.Uniquifier;
import com.google.devtools.build.lib.query2.proto.proto2api.Build;
import com.google.devtools.build.lib.server.FailureDetails.TargetPatterns;
import com.google.devtools.build.lib.vfs.PathFragment;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collection;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.OptionalInt;
import java.util.Set;
import java.util.TreeSet;

/**
 * A query environment over the targets of a {@code bazel query --output=streamed_proto} dump,
 * which runs Bazel's own generic algorithms for every function: it implements neither {@code
 * CustomFunctionQueryEnvironment} nor {@code StreamableQueryEnvironment}, whose functions take
 * their own paths.
 *
 * <p>A target's dependencies are those {@code LabelVisitationUtils.visitTarget} visits, as far
 * as the proto states them. A pattern naming a target, package or repository the dump lacks fails
 * with the message bviz gives for it, the dump holding no BUILD file for Bazel's own.
 */
final class ProtoQueryEnvironment extends AbstractBlazeQueryEnvironment<ProtoQueryEnvironment.Node> {
  /** One target of the dump. */
  static final class Node {
    final Build.Target target;
    final Label label;
    final List<Node> deps = new ArrayList<>();
    final List<Node> rdeps = new ArrayList<>();

    Node(Build.Target target, Label label) {
      this.target = target;
      this.label = label;
    }

    String name() {
      return label.getCanonicalForm();
    }

    boolean isRule() {
      return target.getType() == Build.Target.Discriminator.RULE;
    }

    String kind() {
      return switch (target.getType()) {
        case RULE -> target.getRule().getRuleClass() + " rule";
        case SOURCE_FILE -> "source file";
        case GENERATED_FILE -> "generated file";
        case PACKAGE_GROUP -> "package group";
        default -> target.getType().toString();
      };
    }
  }

  private static final KeyExtractor<Node, String> KEY = Node::name;

  private final Map<PackageIdentifier, Map<String, Node>> packages = new LinkedHashMap<>();
  private final TargetPattern.Parser parser;
  private final Resolver resolver = new Resolver();

  ProtoQueryEnvironment(List<Build.Target> targets, ImmutableMap<String, RepositoryName> mapping)
      throws LabelSyntaxException {
    super(
        /* keepGoing= */ false,
        /* strictScope= */ true,
        label -> true,
        new ExtendedEventHandler() {
          @Override
          public void handle(Event event) {}

          @Override
          public void post(ExtendedEventHandler.Postable post) {}
        },
        ImmutableSet.of(),
        ImmutableList.of(),
        LabelPrinter.legacy());
    parser =
        new TargetPattern.Parser(
            PathFragment.EMPTY_FRAGMENT,
            RepositoryName.MAIN,
            RepositoryMapping.create(mapping, RepositoryName.MAIN));
    Map<String, Node> byName = new HashMap<>();
    for (Build.Target target : targets) {
      Label label = Label.parseCanonical(nameOf(target));
      Node node = new Node(target, label);
      byName.put(node.name(), node);
      packages
          .computeIfAbsent(label.getPackageIdentifier(), unused -> new LinkedHashMap<>())
          .put(label.getName(), node);
    }
    for (Node node : byName.values()) {
      for (String dep : depLabels(node.target)) {
        Node to = byName.get(Label.parseCanonical(dep).getCanonicalForm());
        if (to != null) {
          node.deps.add(to);
          to.rdeps.add(node);
        }
      }
    }
  }

  static String nameOf(Build.Target target) {
    return switch (target.getType()) {
      case RULE -> target.getRule().getName();
      case SOURCE_FILE -> target.getSourceFile().getName();
      case GENERATED_FILE -> target.getGeneratedFile().getName();
      case PACKAGE_GROUP -> target.getPackageGroup().getName();
      default -> throw new IllegalArgumentException(target.toString());
    };
  }

  /** The labels {@code LabelVisitationUtils.visitTarget} visits for {@code target}. */
  private static List<String> depLabels(Build.Target target) {
    return switch (target.getType()) {
      case RULE -> target.getRule().getRuleInputList();
      case SOURCE_FILE -> target.getSourceFile().getVisibilityLabelList();
      case GENERATED_FILE -> List.of(target.getGeneratedFile().getGeneratingRule());
      case PACKAGE_GROUP -> target.getPackageGroup().getIncludedPackageGroupList();
      default -> List.of();
    };
  }

  /** The labels {@code expression} names, sorted, or the message it fails with. */
  String evaluate(QueryExpression expression) throws InterruptedException {
    Set<String> labels = new TreeSet<>();
    OutputFormatterCallback<Node> collect =
        new OutputFormatterCallback<>() {
          @Override
          public void processOutput(Iterable<Node> partialResult) {
            for (Node node : partialResult) {
              labels.add(node.name());
            }
          }
        };
    try {
      evalTopLevelInternal(expression, collect);
    } catch (QueryException e) {
      return "error\t" + e.getMessage();
    }
    return "ok\t" + String.join(" ", labels);
  }

  @Override
  public QueryTaskFuture<Void> getTargetsMatchingPattern(
      QueryExpression owner, String pattern, Callback<Node> callback) {
    try {
      List<Node> found = new ArrayList<>();
      BatchCallback<Node, QueryException> collect = partialResult -> partialResult.forEach(found::add);
      parser
          .parse(pattern)
          .eval(
              resolver,
              () -> IgnoredSubdirectories.EMPTY,
              ImmutableSet.of(),
              collect,
              QueryException.class);
      callback.process(found);
      return immediateSuccessfulFuture(null);
    } catch (TargetParsingException e) {
      return immediateFailedFuture(
          new QueryException(owner, e.getMessage(), e.getDetailedExitCode().getFailureDetail()));
    } catch (QueryException e) {
      return immediateFailedFuture(e);
    } catch (InterruptedException e) {
      return immediateCancelledFuture();
    } catch (Exception e) {
      throw new IllegalStateException(e);
    }
  }

  /** How bviz writes the repository part of a label it names in a message. */
  private static String written(RepositoryName repo) {
    if (repo.isMain()) {
      return "";
    }
    String name = repo.getName();
    return "@" + (name.endsWith("+") ? name.substring(0, name.length() - 1) : name);
  }

  private static TargetParsingException notFound(String message) {
    return new TargetParsingException(message, TargetPatterns.Code.TARGET_PATTERN_PARSE_FAILURE);
  }

  /** Resolves patterns against the dump's packages. */
  private final class Resolver extends TargetPatternResolver<Node> {
    @Override
    public void warn(String msg) {}

    private void checkRepository(RepositoryName repo) throws TargetParsingException {
      if (repo.isMain()) {
        return;
      }
      for (PackageIdentifier id : packages.keySet()) {
        if (id.getRepository().equals(repo)) {
          return;
        }
      }
      String written = repo.isVisible() ? "@@" + repo.getName() : "@" + repo.getName();
      throw notFound("no repository '" + written + "' in the build");
    }

    private Map<String, Node> packageOf(PackageIdentifier id) throws TargetParsingException {
      checkRepository(id.getRepository());
      Map<String, Node> held = packages.get(id);
      if (held == null) {
        throw notFound(
            "no package '"
                + written(id.getRepository())
                + "//"
                + id.getPackageFragment().getPathString()
                + "' in the build");
      }
      return held;
    }

    @Override
    public Node getTargetOrNull(Label label) {
      Map<String, Node> held = packages.get(label.getPackageIdentifier());
      return held == null ? null : held.get(label.getName());
    }

    @Override
    public ResolvedTargets<Node> getExplicitTarget(Label label) throws TargetParsingException {
      Node node = packageOf(label.getPackageIdentifier()).get(label.getName());
      if (node == null) {
        throw notFound(
            "no target '"
                + written(label.getRepository())
                + "//"
                + label.getPackageFragment().getPathString()
                + ":"
                + label.getName()
                + "' in the build");
      }
      return ResolvedTargets.of(node);
    }

    @Override
    public Collection<Node> getTargetsInPackage(
        String originalPattern, PackageIdentifier packageIdentifier, boolean rulesOnly)
        throws TargetParsingException {
      List<Node> found = new ArrayList<>();
      for (Node node : packageOf(packageIdentifier).values()) {
        if (!rulesOnly || node.isRule()) {
          found.add(node);
        }
      }
      return found;
    }

    @Override
    public <E extends Exception & QueryExceptionMarkerInterface> void findTargetsBeneathDirectory(
        RepositoryName repository,
        String originalPattern,
        String directory,
        boolean rulesOnly,
        IgnoredSubdirectories ignoredSubdirectories,
        ImmutableSet<PathFragment> excludedSubdirectories,
        BatchCallback<Node, E> callback,
        Class<E> exceptionClass)
        throws TargetParsingException, E, InterruptedException {
      checkRepository(repository);
      PathFragment beneath = PathFragment.create(directory);
      boolean any = false;
      List<Node> found = new ArrayList<>();
      for (Map.Entry<PackageIdentifier, Map<String, Node>> entry : packages.entrySet()) {
        PackageIdentifier id = entry.getKey();
        if (!id.getRepository().equals(repository)
            || !id.getPackageFragment().startsWith(beneath)) {
          continue;
        }
        for (Node node : entry.getValue().values()) {
          any = true;
          if (!rulesOnly || node.isRule()) {
            found.add(node);
          }
        }
      }
      if (!any) {
        throw notFound("no targets found beneath '" + directory + "'");
      }
      callback.process(found);
    }

    @Override
    public boolean isPackage(PackageIdentifier packageIdentifier) {
      return packages.containsKey(packageIdentifier);
    }

    @Override
    public String getTargetKind(Node node) {
      return node.kind();
    }
  }

  @Override
  public Collection<Node> getSiblingTargetsInPackage(Node node) {
    return packages.get(node.label.getPackageIdentifier()).values();
  }

  @Override
  public Node getOrCreate(Node node) {
    return node;
  }

  @Override
  public Iterable<Node> getFwdDeps(Iterable<Node> nodes, QueryExpressionContext<Node> context) {
    List<Node> deps = new ArrayList<>();
    nodes.forEach(node -> deps.addAll(node.deps));
    return deps;
  }

  @Override
  public Iterable<Node> getReverseDeps(Iterable<Node> nodes, QueryExpressionContext<Node> context) {
    List<Node> rdeps = new ArrayList<>();
    nodes.forEach(node -> rdeps.addAll(node.rdeps));
    return rdeps;
  }

  @Override
  public ThreadSafeMutableSet<Node> getTransitiveClosure(
      ThreadSafeMutableSet<Node> nodes, QueryExpressionContext<Node> context) {
    ThreadSafeMutableSet<Node> closure = createThreadSafeMutableSet();
    ArrayDeque<Node> open = new ArrayDeque<>(nodes);
    while (!open.isEmpty()) {
      Node node = open.pop();
      if (closure.add(node)) {
        open.addAll(node.deps);
      }
    }
    return closure;
  }

  @Override
  public void buildTransitiveClosure(
      QueryExpression caller, ThreadSafeMutableSet<Node> targetNodes, OptionalInt maxDepth) {}

  @Override
  public Iterable<Node> getNodesOnPath(Node from, Node to, QueryExpressionContext<Node> context) {
    Map<Node, Node> reachedFrom = new HashMap<>();
    reachedFrom.put(from, from);
    ArrayDeque<Node> open = new ArrayDeque<>(List.of(from));
    while (!open.isEmpty() && !reachedFrom.containsKey(to)) {
      Node node = open.removeFirst();
      for (Node dep : node.deps) {
        if (reachedFrom.putIfAbsent(dep, node) == null) {
          open.addLast(dep);
        }
      }
    }
    List<Node> path = new ArrayList<>();
    for (Node at = to; at != from; at = reachedFrom.get(at)) {
      path.add(at);
    }
    path.add(from);
    return path;
  }

  @Override
  public ThreadSafeMutableSet<Node> createThreadSafeMutableSet() {
    return new ThreadSafeMutableKeyExtractorBackedSetImpl<>(KEY, Node.class);
  }

  @Override
  public Uniquifier<Node> createUniquifier() {
    return new UniquifierImpl<>(KEY);
  }

  @Override
  public MinDepthUniquifier<Node> createMinDepthUniquifier() {
    return new MinDepthUniquifierImpl<>(KEY, 1);
  }

  @Override
  public TransitiveLoadFilesHelper<Node> getTransitiveLoadFilesHelper() {
    throw new UnsupportedOperationException("the dump holds no BUILD or .bzl files");
  }

  @Override
  public TargetAccessor<Node> getAccessor() {
    return new TargetAccessor<>() {
      @Override
      public String getTargetKind(Node node) {
        return node.kind();
      }

      @Override
      public String getLabel(Node node) {
        return node.name();
      }

      @Override
      public String getPackage(Node node) {
        return node.label.getPackageIdentifier().getCanonicalForm();
      }

      @Override
      public boolean isRule(Node node) {
        return node.isRule();
      }

      @Override
      public boolean isExecutableNonTestRule(Node node) {
        throw new UnsupportedOperationException();
      }

      @Override
      public boolean isTestRule(Node node) {
        throw new UnsupportedOperationException();
      }

      @Override
      public boolean isTestSuite(Node node) {
        throw new UnsupportedOperationException();
      }

      @Override
      public Iterable<Node> getPrerequisites(
          QueryExpression caller, Node node, String attrName, String errorMsgPrefix) {
        throw new UnsupportedOperationException();
      }

      @Override
      public List<String> getStringListAttr(Node node, String attrName) {
        throw new UnsupportedOperationException();
      }

      @Override
      public String getStringAttr(Node node, String attrName) {
        throw new UnsupportedOperationException();
      }

      @Override
      public Iterable<String> getAttrAsString(Node node, String attrName) {
        throw new UnsupportedOperationException();
      }

      @Override
      public ImmutableSet<QueryVisibility<Node>> getVisibility(QueryExpression caller, Node node) {
        throw new UnsupportedOperationException();
      }
    };
  }

  @Override
  public Target getTarget(Label label) {
    throw new UnsupportedOperationException();
  }

  @Override
  public QueryEvalResult evaluateQuery(
      QueryExpression expr, ThreadSafeOutputFormatterCallback<Node> callback) {
    throw new UnsupportedOperationException();
  }

  @Override
  protected void preloadOrThrow(QueryExpression caller, Collection<String> patterns) {}

  @Override
  public void close() {}
}
