package atropos.spring;

import jakarta.servlet.DispatcherType;
import jakarta.servlet.Filter;
import java.io.File;
import org.springframework.boot.autoconfigure.AutoConfiguration;
import org.springframework.boot.web.servlet.FilterRegistrationBean;
import org.springframework.context.annotation.Bean;
import org.springframework.core.Ordered;
import org.springframework.core.annotation.Order;
import org.springframework.security.config.annotation.web.builders.HttpSecurity;
import org.springframework.security.web.SecurityFilterChain;

@AutoConfiguration
public class DocsSecurityAutoConfiguration {

    @Bean
    @Order(Integer.MIN_VALUE)
    public SecurityFilterChain atroposApiDocs(HttpSecurity http) throws Exception {
        return http.securityMatcher("/v3/api-docs", "/v3/api-docs/**", "/v3/api-docs.yaml")
                .authorizeHttpRequests(auth -> auth.anyRequest().permitAll())
                .csrf(csrf -> csrf.disable())
                .build();
    }

    @Bean
    public FilterRegistrationBean<Filter> atroposFuzzGate() {
        FilterRegistrationBean<Filter> registration = new FilterRegistrationBean<>();
        registration.setFilter((request, response, chain) -> {
            boolean enabled = new File("/tmp/spring_fuzz_enabled").isFile();
            // CoverageMap lives in the agent jar. SQL hooks live in the application
            // class loader. Each has its own init.Prepare.
            setPrepareStart(DocsSecurityAutoConfiguration.class.getClassLoader(), enabled);
            setPrepareStart(ClassLoader.getSystemClassLoader(), enabled);
            setPrepareStart(Thread.currentThread().getContextClassLoader(), enabled);
            chain.doFilter(request, response);
        });
        registration.addUrlPatterns("/*");
        registration.setDispatcherTypes(DispatcherType.REQUEST);
        registration.setOrder(Ordered.HIGHEST_PRECEDENCE);
        return registration;
    }

    private static void setPrepareStart(ClassLoader loader, boolean enabled) {
        if (loader == null) {
            return;
        }
        try {
            Class.forName("init.Prepare", true, loader)
                    .getField("start")
                    .set(null, enabled);
        } catch (ReflectiveOperationException ignored) {
            // That loader does not have the Nyx agent or the hooks jar.
        }
    }
}
