allprojects {
    repositories {
        google()
        mavenCentral()
    }
}

val newBuildDir: Directory =
    rootProject.layout.buildDirectory
        .dir("../../build")
        .get()
rootProject.layout.buildDirectory.value(newBuildDir)

subprojects {
    val newSubprojectBuildDir: Directory = newBuildDir.dir(project.name)
    project.layout.buildDirectory.value(newSubprojectBuildDir)
}
subprojects {
    project.evaluationDependsOn(":app")
}

// Setzt compileSdk für alle Plugin-Bibliotheken (wie :file_picker) auf 36,
// damit die AAR-Metadatenprüfung von AGP 9 erfolgreich durchläuft.
subprojects {
    if (project.name != "app") {
        val applyCompileSdk = {
            val android = project.extensions.findByName("android")
            if (android != null) {
                for (m in android.javaClass.methods) {
                    if (m.name == "setCompileSdk" || m.name == "compileSdkVersion" || m.name == "setCompileSdkVersion") {
                        if (m.parameterCount == 1) {
                            val pType = m.parameterTypes[0]
                            if (pType == Int::class.javaPrimitiveType || pType == java.lang.Integer::class.java) {
                                try {
                                    m.invoke(android, 36)
                                } catch (_: Exception) {}
                            }
                        }
                    }
                }
            }
        }

        if (state.executed) {
            applyCompileSdk()
        } else {
            afterEvaluate {
                applyCompileSdk()
            }
        }
    }
}

tasks.register<Delete>("clean") {
    delete(rootProject.layout.buildDirectory)
}
