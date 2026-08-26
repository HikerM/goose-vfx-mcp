plugins {
    kotlin("jvm") version "2.2.21"
    `java-library`
    id("com.vanniktech.maven.publish") version "0.34.0"
}

group = "io.github.hikerm"
version = luminaSdkVersion()

fun luminaSdkVersion(): String {
    val cargoToml = file("../Cargo.toml").readText()
    return Regex("(?m)^version\\s*=\\s*\"([^\"]+)\"")
        .find(cargoToml)
        ?.groupValues
        ?.get(1)
        ?: error("Could not find lumina-sdk version in ../Cargo.toml")
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_11)
    }
}

java {
    sourceCompatibility = JavaVersion.VERSION_11
    targetCompatibility = JavaVersion.VERSION_11
    withSourcesJar()
}

dependencies {
    api("net.java.dev.jna:jna:5.14.0")
    api("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.10.2")
}

tasks.jar {
    manifest {
        attributes(
            "Implementation-Title" to "Lumina SDK",
            "Implementation-Version" to project.version,
        )
    }
}

tasks.withType<GenerateModuleMetadata>().configureEach {
    dependsOn(tasks.named("plainJavadocJar"))
}

mavenPublishing {
    publishToMavenCentral(automaticRelease = true)
    if (providers.gradleProperty("signingInMemoryKey").isPresent) {
        signAllPublications()
    }

    coordinates(
        groupId = "io.github.hikerm",
        artifactId = "gdk",
        version = project.version.toString(),
    )

    pom {
        name.set("Lumina GDK")
        description.set("Kotlin/JVM bindings for the Lumina SDK")
        inceptionYear.set("2026")
        url.set("https://github.com/HikerM/lumina")
        licenses {
            license {
                name.set("Apache License, Version 2.0")
                url.set("https://www.apache.org/licenses/LICENSE-2.0")
                distribution.set("repo")
            }
        }
        developers {
            developer {
                id.set("lumina")
                name.set("Lumina contributors")
            }
        }
        scm {
            connection.set("scm:git:https://github.com/HikerM/lumina.git")
            developerConnection.set("scm:git:ssh://git@github.com/HikerM/lumina.git")
            url.set("https://github.com/HikerM/lumina")
        }
    }
}
