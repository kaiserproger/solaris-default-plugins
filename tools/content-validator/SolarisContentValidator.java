package dev.solaris.loader;

import com.google.gson.Gson;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;

/** Authoring-only Java reference validation; neither native-client nor gameplay admission. */
public final class SolarisContentValidator {
    private SolarisContentValidator() {
    }

    public static void main(String[] args) throws Exception {
        if (args.length != 3) {
            throw new IllegalArgumentException("expected manifest JSON, deployment root and temporary cache");
        }
        byte[] payload = Files.readAllBytes(Path.of(args[0]));
        LoaderManifest declared = new Gson().fromJson(new String(payload, java.nio.charset.StandardCharsets.UTF_8), LoaderManifest.class);
        LoaderManifest manifest = null;
        for (LoaderPlatform platform : LoaderPlatform.values()) {
            // NATIVE is declaration metadata for the native client, not a Java Loader adapter.
            if (platform != LoaderPlatform.NATIVE
                    && declared.bundles().stream().allMatch(bundle -> bundle.loaders().contains(platform))) {
                manifest = LoaderHandshake.inspectManifest(payload, platform, "authoring");
            }
        }
        if (manifest == null) {
            throw new IllegalArgumentException("candidate bundles have no common declared Java Loader adapter");
        }
        Path root = Path.of(args[1]).toRealPath();
        Path cache = Path.of(args[2]);
        for (LoaderBundle bundle : manifest.bundles()) {
            Path artifact = root.resolve(bundle.owner()).resolve(bundle.artifact()).toRealPath();
            if (!artifact.startsWith(root)) {
                throw new IllegalArgumentException("artifact escapes candidate deployment");
            }
            Path destination = LoaderTransferSession.cachePath(cache, bundle);
            Files.createDirectories(destination.getParent());
            Files.copy(artifact, destination, StandardCopyOption.REPLACE_EXISTING);
        }
        // Builds one local immutable result only after every bundle/reference/budget is valid.
        // Nothing is published to a client's current registry or a running server.
        LoaderContentArchive.activate(manifest, cache);
        System.out.println("Java reference Loader content validation passed; native admission not exercised");
    }
}
