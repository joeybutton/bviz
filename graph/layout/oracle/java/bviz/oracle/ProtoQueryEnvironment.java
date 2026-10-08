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
import com.google.devtools.build.lib.packages.AggregatingAttributeMapper;
import com.google.devtools.build.lib.packages.BuildType;
import com.google.devtools.build.lib.packages.LabelConverter;
import com.google.devtools.build.lib.packages.LabelPrinter;
import com.google.devtools.build.lib.packages.RuleVisibility;
import com.google.devtools.build.lib.packages.Target;
import com.google.devtools.build.lib.packages.TargetUtils;
import com.google.devtools.build.lib.packages.TriState;
import com.google.devtools.build.lib.packages.Type;
import com.google.devtools.build.lib.packages.Type.ConversionException;
import com.google.devtools.build.lib.packages.Types;
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
import java.lang.reflect.Constructor;
import java.lang.reflect.Method;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Collection;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.OptionalInt;
import java.util.Set;
import java.util.TreeSet;
import net.starlark.java.eval.Starlark;
import net.starlark.java.eval.StarlarkInt;

/**
 * A query environment over the targets of a {@code bazel query --output=streamed_proto} dump,
 * which runs Bazel's own generic algorithms for every function: it implements neither {@code
 * CustomFunctionQueryEnvironment} nor {@code StreamableQueryEnvironment}, whose functions take
 * their own paths.
 *
 * <p>A target's dependencies are those {@code LabelVisitationUtils.visitTarget} visits, as far
 * as the proto states them, under {@code DependencyFilter.NO_IMPLICIT_DEPS} where asked. A
 * pattern naming a target, package or repository the dump lacks fails with the message bviz
 * gives for it, the dump holding no BUILD file for Bazel's own.
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

  ProtoQueryEnvironment(
      List<Build.Target> targets,
      ImmutableMap<String, RepositoryName> mapping,
      boolean noImplicitDeps)
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
      for (String dep : depLabels(node.target, byName, noImplicitDeps)) {
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

  /**
   * The labels {@code LabelVisitationUtils.visitTarget} visits for {@code target}, an output
   * file's visibility being that of its generating rule, found among {@code byName}. Under {@code
   * noImplicitDeps} a rule's are those its explicitly specified attributes name, and its
   * visibility's where its BUILD file sets that, which is what the filter keeps of {@code
   * visitRule} and {@code visitRuleVisibility}; the rest is visited through no attribute.
   */
  private static List<String> depLabels(
      Build.Target target, Map<String, Node> byName, boolean noImplicitDeps) {
    return switch (target.getType()) {
      case RULE -> {
        Build.Rule rule = target.getRule();
        List<String> deps =
            new ArrayList<>(noImplicitDeps ? explicitDeps(rule) : rule.getRuleInputList());
        // A drawn attribute may name its own rule, which as a dependency is a cycle Bazel
        // refuses to load, so a dump's rule inputs never hold it.
        String self = Label.parseCanonicalUnchecked(rule.getName()).getCanonicalForm();
        deps.removeIf(self::equals);
        if (!noImplicitDeps || visibilityExplicit(rule)) {
          deps.addAll(visibilityDeps(rule));
        }
        yield deps;
      }
      case SOURCE_FILE -> target.getSourceFile().getPackageGroupList();
      case GENERATED_FILE -> {
        String rule = target.getGeneratedFile().getGeneratingRule();
        List<String> deps = new ArrayList<>(List.of(rule));
        Node generating = byName.get(Label.parseCanonicalUnchecked(rule).getCanonicalForm());
        if (generating != null && generating.isRule()) {
          deps.addAll(visibilityDeps(generating.target.getRule()));
        }
        yield deps;
      }
      case PACKAGE_GROUP -> target.getPackageGroup().getIncludedPackageGroupList();
      default -> List.of();
    };
  }

  private static final Method PARSE_VISIBILITY;

  static {
    try {
      PARSE_VISIBILITY = RuleVisibility.class.getDeclaredMethod("parse", List.class);
      PARSE_VISIBILITY.setAccessible(true);
    } catch (ReflectiveOperationException e) {
      throw new ExceptionInInitializerError(e);
    }
  }

  private static boolean visibilityExplicit(Build.Rule rule) {
    return rule.getAttributeList().stream()
        .anyMatch(a -> a.getName().equals("visibility") && a.getExplicitlySpecified());
  }

  /**
   * The labels {@code AggregatingAttributeMapper.visitLabels} visits of {@code rule}'s explicitly
   * specified attributes: every label of a value, through Bazel's own types, and of a select
   * every condition but the default and every branch set, visited as {@code visitLabelsInSelect}
   * does; none of an output or nodep attribute. A branch of {@code None} takes the type's default,
   * which names no label of any type drawn.
   */
  private static List<String> explicitDeps(Build.Rule rule) {
    List<String> deps = new ArrayList<>();
    Type.LabelVisitor visitor = (label, attribute) -> deps.add(label.getCanonicalForm());
    for (Build.Attribute attribute : rule.getAttributeList()) {
      if (!attribute.getExplicitlySpecified() || attribute.getNodep()) {
        continue;
      }
      try {
        if (attribute.getType() == Build.Attribute.Discriminator.SELECTOR_LIST) {
          @SuppressWarnings("unchecked")
          Type<Object> type =
              (Type<Object>) type(attribute.getSelectorList().getType(), false);
          for (Object built : selectors(attribute, type)) {
            @SuppressWarnings("unchecked")
            BuildType.Selector<Object> selector = (BuildType.Selector<Object>) built;
            boolean hasDefault = selector.hasDefault();
            boolean unconditional = selector.isUnconditional();
            selector.forEach(
                (key, value) -> {
                  if (!unconditional
                      && (!hasDefault || !BuildType.Selector.isDefaultConditionLabel(key))) {
                    visitor.visit(key, null);
                  }
                  if (selector.isValueSet(key)) {
                    type.visitLabels(visitor, value, null);
                  }
                });
          }
        } else {
          Type<?> type = type(attribute.getType(), false);
          if (type == BuildType.OUTPUT || type == BuildType.OUTPUT_LIST) {
            continue;
          }
          Object value = stated(attribute.getType(), new AttributeFields(attribute));
          if (value != null) {
            visit(type, type.convert(value, attribute.getName()), visitor);
          }
        }
      } catch (ReflectiveOperationException | ConversionException | LabelSyntaxException e) {
        throw new IllegalStateException(attribute.toString(), e);
      }
    }
    return deps;
  }

  private static <T> void visit(Type<T> type, Object value, Type.LabelVisitor visitor) {
    type.visitLabels(visitor, type.cast(value), null);
  }

  /**
   * The package groups {@code rule}'s visibility names, as Bazel's own {@code RuleVisibility}
   * parses them (reached reflectively, being package-private).
   */
  private static List<String> visibilityDeps(Build.Rule rule) {
    for (Build.Attribute attribute : rule.getAttributeList()) {
      if (attribute.getName().equals("visibility")) {
        List<Label> labels =
            attribute.getStringListValueList().stream().map(Label::parseCanonicalUnchecked).toList();
        try {
          RuleVisibility visibility = (RuleVisibility) PARSE_VISIBILITY.invoke(null, labels);
          return visibility.getDependencyLabels().stream().map(Label::getCanonicalForm).toList();
        } catch (ReflectiveOperationException e) {
          throw new IllegalStateException(attribute.toString(), e);
        }
      }
    }
    return List.of();
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

      /**
       * The values {@code TargetUtils.getAttrAsString} prints for an attribute the proto states,
       * each converted to the attribute's type and printed by Bazel's own code.
       */
      @Override
      public Iterable<String> getAttrAsString(Node node, String attrName) {
        if (attrName.equals("name")) {
          return List.of(node.label.getName());
        }
        for (Build.Attribute attribute : node.target.getRule().getAttributeList()) {
          if (attribute.getName().equals(attrName)) {
            return printed(attribute);
          }
        }
        return List.of();
      }

      @Override
      public ImmutableSet<QueryVisibility<Node>> getVisibility(QueryExpression caller, Node node) {
        throw new UnsupportedOperationException();
      }
    };
  }

  private static final Constructor<?> SELECTOR;
  private static final Method ALL_VALUES;

  static {
    try {
      SELECTOR =
          BuildType.Selector.class.getDeclaredConstructor(
              ImmutableMap.class, Object.class, LabelConverter.class, Type.class);
      SELECTOR.setAccessible(true);
      ALL_VALUES =
          AggregatingAttributeMapper.class.getDeclaredMethod(
              "getAllValues", List.class, Type.class, boolean.class);
      ALL_VALUES.setAccessible(true);
    } catch (ReflectiveOperationException e) {
      throw new ExceptionInInitializerError(e);
    }
  }

  /**
   * What {@code TargetUtils.getAttrAsString} returns for {@code attribute}: each value {@code
   * AggregatingAttributeMapper.visitAttribute} visits, printed by {@code
   * TargetUtils.convertAttributeValue}. A select is rebuilt as Bazel's own {@code Selector}s, whose
   * combinations Bazel's {@code getAllValues} visits.
   */
  private static List<String> printed(Build.Attribute attribute) {
    try {
      List<String> printed = new ArrayList<>();
      if (attribute.getType() == Build.Attribute.Discriminator.SELECTOR_LIST) {
        Type<?> type = type(attribute.getSelectorList().getType(), attribute.getNodep());
        List<Object> selectors = selectors(attribute, type);
        for (Object value : (List<?>) ALL_VALUES.invoke(null, selectors, type, false)) {
          printed.add(TargetUtils.convertAttributeValue(type, value));
        }
      } else {
        Type<?> type = type(attribute.getType(), attribute.getNodep());
        Object value = stated(attribute.getType(), new AttributeFields(attribute));
        if (value != null) {
          Object converted = converting(type).convert(value, attribute.getName());
          printed.add(TargetUtils.convertAttributeValue(type, converted));
        }
      }
      return printed;
    } catch (ReflectiveOperationException | ConversionException | LabelSyntaxException e) {
      throw new IllegalStateException(attribute.toString(), e);
    }
  }

  /** The selects {@code attribute} concatenates, rebuilt as Bazel's own {@code Selector}s. */
  private static List<Object> selectors(Build.Attribute attribute, Type<?> type)
      throws ReflectiveOperationException, LabelSyntaxException {
    Build.Attribute.SelectorList list = attribute.getSelectorList();
    List<Object> selectors = new ArrayList<>();
    for (Build.Attribute.Selector selector : list.getElementsList()) {
      ImmutableMap.Builder<Object, Object> branches = ImmutableMap.builder();
      for (Build.Attribute.SelectorEntry entry : selector.getEntriesList()) {
        Object value =
            entry.getIsDefaultValue()
                ? Starlark.NONE
                : stated(list.getType(), new EntryFields(entry));
        branches.put(Label.parseCanonical(entry.getLabel()), value);
      }
      selectors.add(SELECTOR.newInstance(branches.buildOrThrow(), attribute.getName(), null, type));
    }
    return selectors;
  }

  /**
   * The type that converts a value of {@code type} as written. An output type checks its label
   * against the package of the rule converting it, which a value read without its rule cannot name,
   * so its labels convert as any other label.
   */
  private static Type<?> converting(Type<?> type) {
    if (type == BuildType.OUTPUT) {
      return BuildType.LABEL;
    }
    if (type == BuildType.OUTPUT_LIST) {
      return BuildType.LABEL_LIST;
    }
    return type;
  }

  /** The type of an attribute the proto states as {@code kind}, a label one where {@code nodep}. */
  private static Type<?> type(Build.Attribute.Discriminator kind, boolean nodep) {
    return switch (kind) {
      case INTEGER -> Type.INTEGER;
      case STRING -> nodep ? BuildType.NODEP_LABEL : Type.STRING;
      case LABEL -> BuildType.LABEL;
      case OUTPUT -> BuildType.OUTPUT;
      case STRING_LIST -> nodep ? BuildType.NODEP_LABEL_LIST : Types.STRING_LIST;
      case DISTRIBUTION_SET -> Types.STRING_SET;
      case LABEL_LIST -> BuildType.LABEL_LIST;
      case OUTPUT_LIST -> BuildType.OUTPUT_LIST;
      case LICENSE -> BuildType.LICENSE;
      case STRING_DICT -> Types.STRING_DICT;
      case LABEL_LIST_DICT -> BuildType.LABEL_LIST_DICT;
      case STRING_LIST_DICT -> Types.STRING_LIST_DICT;
      case BOOLEAN -> Type.BOOLEAN;
      case TRISTATE -> BuildType.TRISTATE;
      case INTEGER_LIST -> Types.INTEGER_LIST;
      case LABEL_DICT_UNARY -> BuildType.LABEL_DICT_UNARY;
      case LABEL_KEYED_STRING_DICT -> BuildType.LABEL_KEYED_STRING_DICT;
      default -> throw new UnsupportedOperationException(kind.toString());
    };
  }

  /** The value fields an attribute and a select branch share. */
  private interface Fields {
    boolean hasInt();

    int intValue();

    boolean hasString();

    String stringValue();

    boolean hasBoolean();

    boolean booleanValue();

    boolean hasTristate();

    Build.Attribute.Tristate tristate();

    List<String> strings();

    List<Integer> ints();

    boolean hasLicense();

    Build.License license();

    List<Build.StringDictEntry> stringDict();

    List<Build.StringListDictEntry> stringListDict();

    List<Build.LabelDictUnaryEntry> labelDict();

    List<Build.LabelKeyedStringDictEntry> labelKeyedStringDict();
  }

  private record AttributeFields(Build.Attribute a) implements Fields {
    public boolean hasInt() { return a.hasIntValue(); }
    public int intValue() { return a.getIntValue(); }
    public boolean hasString() { return a.hasStringValue(); }
    public String stringValue() { return a.getStringValue(); }
    public boolean hasBoolean() { return a.hasBooleanValue(); }
    public boolean booleanValue() { return a.getBooleanValue(); }
    public boolean hasTristate() { return a.hasTristateValue(); }
    public Build.Attribute.Tristate tristate() { return a.getTristateValue(); }
    public List<String> strings() { return a.getStringListValueList(); }
    public List<Integer> ints() { return a.getIntListValueList(); }
    public boolean hasLicense() { return a.hasLicense(); }
    public Build.License license() { return a.getLicense(); }
    public List<Build.StringDictEntry> stringDict() { return a.getStringDictValueList(); }
    public List<Build.StringListDictEntry> stringListDict() { return a.getStringListDictValueList(); }
    public List<Build.LabelDictUnaryEntry> labelDict() { return a.getLabelDictUnaryValueList(); }
    public List<Build.LabelKeyedStringDictEntry> labelKeyedStringDict() {
      return a.getLabelKeyedStringDictValueList();
    }
  }

  private record EntryFields(Build.Attribute.SelectorEntry e) implements Fields {
    public boolean hasInt() { return e.hasIntValue(); }
    public int intValue() { return e.getIntValue(); }
    public boolean hasString() { return e.hasStringValue(); }
    public String stringValue() { return e.getStringValue(); }
    public boolean hasBoolean() { return e.hasBooleanValue(); }
    public boolean booleanValue() { return e.getBooleanValue(); }
    public boolean hasTristate() { return e.hasTristateValue(); }
    public Build.Attribute.Tristate tristate() { return e.getTristateValue(); }
    public List<String> strings() { return e.getStringListValueList(); }
    public List<Integer> ints() { return e.getIntListValueList(); }
    public boolean hasLicense() { return e.hasLicense(); }
    public Build.License license() { return e.getLicense(); }
    public List<Build.StringDictEntry> stringDict() { return e.getStringDictValueList(); }
    public List<Build.StringListDictEntry> stringListDict() { return e.getStringListDictValueList(); }
    public List<Build.LabelDictUnaryEntry> labelDict() { return e.getLabelDictUnaryValueList(); }
    public List<Build.LabelKeyedStringDictEntry> labelKeyedStringDict() {
      return e.getLabelKeyedStringDictValueList();
    }
  }

  /**
   * The value {@code fields} state for an attribute of type {@code kind}, in a form the type's
   * {@code convert} reads, labels still as written; {@code null} where they state none.
   */
  private static Object stated(Build.Attribute.Discriminator kind, Fields fields) {
    return switch (kind) {
      case INTEGER -> fields.hasInt() ? StarlarkInt.of(fields.intValue()) : null;
      case STRING, LABEL, OUTPUT -> fields.hasString() ? fields.stringValue() : null;
      case STRING_LIST, LABEL_LIST, OUTPUT_LIST -> fields.strings();
      case DISTRIBUTION_SET -> new LinkedHashSet<>(fields.strings());
      case INTEGER_LIST -> fields.ints().stream().map(StarlarkInt::of).toList();
      case BOOLEAN -> fields.hasBoolean() ? fields.booleanValue() : null;
      case TRISTATE ->
          fields.hasTristate() ? TriState.valueOf(fields.tristate().name()) : null;
      case STRING_DICT, LABEL_DICT_UNARY, LABEL_KEYED_STRING_DICT -> {
        Map<String, String> dict = new LinkedHashMap<>();
        fields.stringDict().forEach(entry -> dict.put(entry.getKey(), entry.getValue()));
        fields.labelDict().forEach(entry -> dict.put(entry.getKey(), entry.getValue()));
        fields
            .labelKeyedStringDict()
            .forEach(entry -> dict.put(entry.getKey(), entry.getValue()));
        yield dict;
      }
      case STRING_LIST_DICT, LABEL_LIST_DICT -> {
        Map<String, List<String>> dict = new LinkedHashMap<>();
        fields.stringListDict().forEach(entry -> dict.put(entry.getKey(), entry.getValueList()));
        yield dict;
      }
      case LICENSE -> {
        if (!fields.hasLicense()) {
          yield null;
        }
        List<String> written = new ArrayList<>(fields.license().getLicenseTypeList());
        fields.license().getExceptionList().forEach(label -> written.add("exception=" + label));
        yield written;
      }
      default -> throw new UnsupportedOperationException(kind.toString());
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
