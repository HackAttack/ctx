plugins {
    `java-library`
}

group = "rs.ctx"
version = "0.1.0-experimental"

java {
    sourceCompatibility = JavaVersion.VERSION_11
    targetCompatibility = JavaVersion.VERSION_11
}

tasks.withType<JavaCompile>().configureEach {
    options.release.set(11)
}

