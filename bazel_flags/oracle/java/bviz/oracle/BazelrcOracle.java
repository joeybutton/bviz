package bviz.oracle;

import static java.nio.charset.StandardCharsets.UTF_8;

import com.google.common.collect.ImmutableList;
import com.google.common.collect.ImmutableMap;
import com.google.devtools.build.lib.analysis.ConfiguredRuleClassProvider;
import com.google.devtools.build.lib.bazel.Bazel;
import com.google.devtools.build.lib.runtime.BlazeCommand;
import com.google.devtools.build.lib.runtime.BlazeCommandUtils;
import com.google.devtools.build.lib.runtime.BlazeModule;
import com.google.devtools.build.lib.runtime.Command;
import com.google.devtools.build.lib.runtime.OptionsSupplier;
import com.google.devtools.build.lib.runtime.commands.AqueryCommand;
import com.google.devtools.build.lib.runtime.commands.BuildCommand;
import com.google.devtools.build.lib.runtime.commands.CqueryCommand;
import com.google.devtools.build.lib.runtime.commands.QueryCommand;
import com.google.devtools.build.lib.runtime.commands.TestCommand;
import com.google.devtools.common.options.OptionPriority.PriorityCategory;
import com.google.devtools.common.options.OptionValueDescription;
import com.google.devtools.common.options.OptionsParser;
import com.google.devtools.common.options.OptionsParsingException;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.logging.Level;
import java.util.logging.Logger;
import java.util.stream.Stream;

/**
 * Parses each config of a bazelrc read from stdin as Bazel expands {@code --config} for each
 * command named as an argument, and prints a tab-separated line per command and config: {@code
 * <command> <config> ok}, followed by {@code <flag>=<value>} for each of {@link #CAPTURES} the
 * command takes, or {@code <command> <config> rejected <message>} with the parser's error or
 * warnings. A section for a command the oracle does not model is printed as {@code <command>
 * <config> unchecked command}.
 *
 * <p>A command's options are those {@link BlazeCommandUtils#getOptions} assembles from Bazel's own
 * modules, as the server does, and a config's lines are taken from the sections of the command
 * and of every command it inherits options from, bases first, as Bazel's option handler takes
 * them. The rc file is read as plain {@code command:config flag...} lines and comments, without
 * the client's quoting, {@code import} or {@code try-import}.
 */
public final class BazelrcOracle {
  private static final ImmutableMap<String, Class<? extends BlazeCommand>> COMMANDS =
      Stream.of(
              BuildCommand.class,
              TestCommand.class,
              QueryCommand.class,
              CqueryCommand.class,
              AqueryCommand.class)
          .collect(
              ImmutableMap.toImmutableMap(c -> c.getAnnotation(Command.class).name(), c -> c));

  /**
   * The flags naming a file a command writes, printed as the config resolves them, empty where
   * unset.
   */
  private static final ImmutableList<String> CAPTURES =
      ImmutableList.of("profile", "build_event_binary_file", "execution_log_compact_file");

  public static void main(String[] args) {
    // Bazel's modules start a thread that outlives main, so the process is ended here.
    try {
      run(args);
    } catch (Throwable e) {
      e.printStackTrace();
      System.exit(1);
    }
    System.exit(0);
  }

  private static void run(String[] commands) throws Exception {
    Logger.getLogger("").setLevel(Level.OFF);
    Map<String, Map<String, List<String>>> sections = readSections();
    List<BlazeModule> modules = new ArrayList<>();
    for (Class<? extends BlazeModule> module : Bazel.BAZEL_MODULES) {
      modules.add(module.getConstructor().newInstance());
    }
    ConfiguredRuleClassProvider.Builder rules = new ConfiguredRuleClassProvider.Builder();
    for (BlazeModule module : modules) {
      module.initializeRuleClasses(rules);
    }
    ConfiguredRuleClassProvider ruleClassProvider = rules.build();
    List<OptionsSupplier> suppliers = List.copyOf(modules);

    PrintStream out = new PrintStream(System.out, true, UTF_8);
    sections.forEach(
        (command, configs) -> {
          if (!COMMANDS.containsKey(command)) {
            for (String config : configs.keySet()) {
              out.println(command + "\t" + config + "\tunchecked command");
            }
          }
        });
    for (String name : commands) {
      Class<? extends BlazeCommand> command = COMMANDS.get(name);
      if (command == null) {
        throw new IllegalArgumentException("no command " + name);
      }
      List<String> inherited = new ArrayList<>();
      addInheritance(command.getAnnotation(Command.class), inherited);
      Map<String, List<String>> configs = new LinkedHashMap<>();
      for (String section : inherited) {
        sections
            .getOrDefault(section, Map.of())
            .forEach(
                (config, flags) ->
                    configs.computeIfAbsent(config, c -> new ArrayList<>()).addAll(flags));
      }
      for (var config : configs.entrySet()) {
        OptionsParser parser =
            OptionsParser.builder()
                .optionsClasses(BlazeCommandUtils.getOptions(command, suppliers, ruleClassProvider))
                .allowResidue(false)
                .build();
        String verdict;
        try {
          parser.parse(PriorityCategory.RC_FILE, "bazelrc", config.getValue());
          verdict =
              parser.getWarnings().isEmpty()
                  ? "ok" + captures(parser)
                  : "rejected\t" + String.join("; ", parser.getWarnings());
        } catch (OptionsParsingException e) {
          verdict = "rejected\t" + e.getMessage();
        }
        out.println(name + "\t" + config.getKey() + "\t" + verdict);
      }
    }
  }

  /** A tab-led {@code <flag>=<value>} for each of {@link #CAPTURES} the parser's command takes. */
  private static String captures(OptionsParser parser) throws OptionsParsingException {
    StringBuilder captures = new StringBuilder();
    for (String name : CAPTURES) {
      if (parser.getOptionDescription(name) == null) {
        continue;
      }
      OptionValueDescription value = parser.getOptionValueDescription(name);
      captures
          .append('\t')
          .append(name)
          .append('=')
          .append(value == null ? "" : Objects.toString(value.getValue(), ""));
    }
    return captures.toString();
  }

  /** The flags of each section of the bazelrc on stdin, by command and then config, in order. */
  private static Map<String, Map<String, List<String>>> readSections() throws Exception {
    Map<String, Map<String, List<String>>> sections = new LinkedHashMap<>();
    BufferedReader in = new BufferedReader(new InputStreamReader(System.in, UTF_8));
    for (String line = in.readLine(); line != null; line = in.readLine()) {
      line = line.strip();
      if (line.isEmpty() || line.startsWith("#")) {
        continue;
      }
      List<String> words = List.of(line.split("\\s+"));
      String[] section = words.get(0).split(":", 2);
      String config = section.length == 2 ? section[1] : "";
      sections
          .computeIfAbsent(section[0], c -> new LinkedHashMap<>())
          .computeIfAbsent(config, c -> new ArrayList<>())
          .addAll(words.subList(1, words.size()));
    }
    return sections;
  }

  /** Adds the names of {@code command} and the commands it inherits options from, bases first. */
  private static void addInheritance(Command command, List<String> names) {
    for (Class<? extends BlazeCommand> base : command.inheritsOptionsFrom()) {
      addInheritance(base.getAnnotation(Command.class), names);
    }
    names.add(command.name());
  }
}
