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
import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.io.Reader;
import java.io.Writer;
import java.util.stream.Stream;

/**
 * Answers query expressions with Bazel's own query engine, each read from stdin and answered on
 * stdout as a record ending in NUL, which no expression holds where a line break may: {@code
 * parse\t<expression>} with the parsed expression printed back, or {@code error\t<message>}.
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

  public static void main(String[] args) throws IOException {
    BufferedReader in = new BufferedReader(new InputStreamReader(System.in, ISO_8859_1));
    try (Writer out = new BufferedWriter(new OutputStreamWriter(System.out, ISO_8859_1))) {
      for (String record = nextRecord(in); record != null; record = nextRecord(in)) {
        out.write(answer(record));
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
