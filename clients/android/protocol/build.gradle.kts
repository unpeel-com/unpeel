plugins {
    kotlin("jvm")
}

java {
    toolchain {
        languageVersion.set(JavaLanguageVersion.of(17))
    }
}

dependencies {
    implementation("org.bouncycastle:bcprov-jdk18on:1.78.1")
    compileOnly("org.json:json:20240303")
    testImplementation("org.json:json:20240303")
    testImplementation("junit:junit:4.13.2")
}

// Pin Relay KAT + host-capabilities to this checkout's protocol/ (or
// UNPEEL_PROTOCOL_DIR), matching UnpeelShared's UNPEEL_PROTOCOL_DIR override.
tasks.named<Test>("test") {
    val protocolDir = System.getenv("UNPEEL_PROTOCOL_DIR")
        ?: rootProject.projectDir.resolve("../../protocol").canonicalFile.absolutePath
    systemProperty("unpeel.protocol.dir", protocolDir)
}
