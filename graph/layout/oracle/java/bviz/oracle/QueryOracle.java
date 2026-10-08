package bviz.oracle;

import static java.nio.charset.StandardCharsets.ISO_8859_1;

import com.google.common.collect.ImmutableMap;
import com.google.devtools.build.lib.query2.RBuildFilesFunction;
import com.google.devtools.build.lib.query2.cquery.ConfigFunction;
import com.google.devtools.build.lib.query2.engine.AllRdepsFunction;
import com.google.devtools.build.lib.query2.engine.InputsFunction;
import com.google.devtools.build.lib.query2.engine.MnemonicFunction;
import com.google.devtools.build.lib.query2.engine.OutputsFunction;
import com.google.devtools.build.lib.query2.engine.QueryEnvironment;
import com.google.devtools.build.lib.query2.engine.QueryEnvironment.QueryFunction;
import com.google.devtools.build.lib.query2.engine.QueryExpression;
import com.google.devtools.build.lib.query2.engine.QueryParser;
import com.google.devtools.build.lib.query2.engine.QuerySyntaxException;
import com.google.devtools.build.lib.cmdline.Label;
import com.google.devtools.build.lib.cmdline.RepositoryName;
import com.google.devtools.build.lib.query2.proto.proto2api.Build;
import java.io.BufferedInputStream;
import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.io.Reader;
import java.io.Writer;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.logging.Level;
import java.util.logging.Logger;
import java.util.stream.Stream;

/**
 * Answers query expressions with Bazel's own query engine, each read from stdin and answered on
 * stdout as a record ending in NUL, which no expression holds where a line break may.
 *
 * <p>Run bare, it parses each expression: {@code parse\t<expression>} with the parsed expression
 * printed back, or {@code error\t<message>}. Run with the path of a {@code bazel query
 * --output=streamed_proto} dump, it evaluates each over the dump's targets: {@code ok\t<labels>}
 * with the labels named, sorted and separated by spaces, or {@code error\t<message>}. A further
 * {@code --noimplicit_deps} walks the explicit dependencies alone.
 *
 * <p>Bazel holds strings internally as one char per UTF-8 byte, so stdin and stdout are read and
 * written as ISO-8859-1 to hand the engine its own encoding.
 */
public final class QueryOracle {
  private static final char RECORD_END = '\0';

  private static final ImmutableMap<String, QueryFunction> FUNCTIONS =
      Stream.concat(
              QueryEnvironment.DEFAULT_QUERY_FUNCTIONS.stream(),
              Stream.of(
                  new AllRdepsFunction(),
                  new RBuildFilesFunction(),
                  new ConfigFunction(),
                  new MnemonicFunction(),
                  new InputsFunction(),
                  new OutputsFunction()))
          .collect(ImmutableMap.toImmutableMap(QueryFunction::getName, f -> f));

  public static void main(String[] args) throws Exception {
    // Guava logs each failure a combined future drops after its first, which the engine
    // reports on its own.
    Logger.getLogger("").setLevel(Level.OFF);
    boolean noImplicitDeps = List.of(args).contains("--noimplicit_deps");
    ProtoQueryEnvironment env =
        args.length == 0 ? null : environment(Path.of(args[0]), noImplicitDeps);
    BufferedReader in = new BufferedReader(new InputStreamReader(System.in, ISO_8859_1));
    try (Writer out = new BufferedWriter(new OutputStreamWriter(System.out, ISO_8859_1))) {
      for (String record = nextRecord(in); record != null; record = nextRecord(in)) {
        out.write(env == null ? answer(record) : evaluate(env, record));
        out.write(RECORD_END);
      }
    }
  }

  /** The next record from {@code in} without its end, or null once {@code in} is exhausted. */
  private static String nextRecord(Reader in) throws IOException {
    StringBuilder record = new StringBuilder();
    for (int c = in.read(); c != RECORD_END; c = in.read()) {
      if (c == -1) {
        return record.isEmpty() ? null : record.toString();
      }
      record.append((char) c);
    }
    return record.toString();
  }

  /**
   * The environment over the dump at {@code path}. Each repository named {@code <module>+}, as
   * Bzlmod names a module's, is visible as {@code @<module>}.
   */
  private static ProtoQueryEnvironment environment(Path path, boolean noImplicitDeps)
      throws Exception {
    List<Build.Target> targets = new ArrayList<>();
    Map<String, RepositoryName> mapping = new HashMap<>();
    try (InputStream in = new BufferedInputStream(Files.newInputStream(path))) {
      for (Build.Target target = Build.Target.parseDelimitedFrom(in);
          target != null;
          target = Build.Target.parseDelimitedFrom(in)) {
        targets.add(target);
      }
    }
    for (Build.Target target : targets) {
      String repo = Label.parseCanonical(ProtoQueryEnvironment.nameOf(target)).getRepository().getName();
      if (repo.endsWith("+") && repo.indexOf('+') == repo.length() - 1) {
        mapping.put(repo.substring(0, repo.length() - 1), RepositoryName.createUnvalidated(repo));
      }
    }
    return new ProtoQueryEnvironment(targets, ImmutableMap.copyOf(mapping), noImplicitDeps);
  }

  private static String evaluate(ProtoQueryEnvironment env, String expression)
      throws InterruptedException {
    try {
      return env.evaluate(QueryParser.parse(expression, FUNCTIONS));
    } catch (QuerySyntaxException e) {
      return "error\t" + e.getMessage();
    }
  }

  private static String answer(String expression) {
    try {
      QueryExpression parsed = QueryParser.parse(expression, FUNCTIONS);
      return "parse\t" + parsed;
    } catch (QuerySyntaxException e) {
      return "error\t" + e.getMessage();
    }
  }

  private QueryOracle() {}
}
