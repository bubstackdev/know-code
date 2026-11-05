// Lists a Spring WebFlux module's request mappings by Spring's own handler mapping (eval/routes-v1/README.md, "Java and
// Kotlin"): for each class under the given class directories that RequestMappingHandlerMapping takes for a handler, the
// mappings its getMappingForMethod gives, as MethodIntrospector reads them. One JSON line per HTTP method and path
// pattern. Compiled and run against the module's own runtime classpath, offline, with no application started.
import java.io.File;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Collection;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeSet;
import java.util.stream.Stream;

import org.springframework.context.support.StaticApplicationContext;
import org.springframework.core.MethodIntrospector;
import org.springframework.web.reactive.result.method.RequestMappingInfo;
import org.springframework.web.reactive.result.method.annotation.RequestMappingHandlerMapping;

public class KcFluxMappings extends RequestMappingHandlerMapping {
    public static void main(String[] args) throws Exception {
        KcFluxMappings m = new KcFluxMappings();
        StaticApplicationContext ctx = new StaticApplicationContext();
        ctx.refresh();
        m.setApplicationContext(ctx);
        m.afterPropertiesSet();
        ClassLoader cl = KcFluxMappings.class.getClassLoader();
        Set<String> out = new TreeSet<>();
        for (String dir : args) {
            for (String name : classNames(Paths.get(dir))) {
                Class<?> type;
                try {
                    type = Class.forName(name, false, cl);
                } catch (Throwable t) {
                    System.err.println("not loaded: " + name + ": " + t);
                    continue;
                }
                if (type.isInterface() || Modifier.isAbstract(type.getModifiers()) || type.isAnnotation()) {
                    continue;
                }
                boolean handler;
                try {
                    handler = m.isHandler(type);
                } catch (Throwable t) {
                    System.err.println("not read: " + name + ": " + t);
                    continue;
                }
                if (!handler) {
                    continue;
                }
                final Class<?> t = type;
                Map<Method, RequestMappingInfo> found =
                    MethodIntrospector.selectMethods(t, (MethodIntrospector.MetadataLookup<RequestMappingInfo>) meth -> m.getMappingForMethod(meth, t));
                for (Map.Entry<Method, RequestMappingInfo> e : found.entrySet()) {
                    RequestMappingInfo info = e.getValue();
                    List<String> verbs = new ArrayList<>();
                    for (Object v : info.getMethodsCondition().getMethods()) {
                        verbs.add(v.toString());
                    }
                    if (verbs.isEmpty()) {
                        verbs.add("*");
                    }
                    for (String path : patterns(info)) {
                        for (String verb : verbs) {
                            out.add("{\"method\": " + json(verb) + ", \"path\": " + json(path) + ", \"handler\": "
                                + json(type.getName() + "#" + e.getKey().getName()) + "}");
                        }
                    }
                }
            }
        }
        out.forEach(System.out::println);
    }

    /** The mapping's path patterns as written, by whichever accessor the module's Spring version has. */
    static List<String> patterns(Object info) throws Exception {
        Collection<?> found;
        try {
            found = (Collection<?>) info.getClass().getMethod("getPatternValues").invoke(info);
        } catch (NoSuchMethodException e) {
            Object condition = info.getClass().getMethod("getPatternsCondition").invoke(info);
            found = (Collection<?>) condition.getClass().getMethod("getPatterns").invoke(condition);
        }
        List<String> out = new ArrayList<>();
        for (Object p : found) {
            out.add(p.toString());
        }
        return out;
    }

    static List<String> classNames(Path root) throws Exception {
        List<String> names = new ArrayList<>();
        if (!Files.isDirectory(root)) {
            return names;
        }
        try (Stream<Path> files = Files.walk(root)) {
            files.filter(p -> p.toString().endsWith(".class")).forEach(p -> {
                String rel = root.relativize(p).toString();
                String name = rel.substring(0, rel.length() - ".class".length()).replace(File.separatorChar, '.');
                if (!name.endsWith("module-info") && !name.endsWith("package-info")) {
                    names.add(name);
                }
            });
        }
        names.sort(null);
        return names;
    }

    static String json(String s) {
        StringBuilder b = new StringBuilder("\"");
        for (char c : s.toCharArray()) {
            if (c == '"' || c == '\\') {
                b.append('\\');
            }
            b.append(c);
        }
        return b.append('"').toString();
    }
}
