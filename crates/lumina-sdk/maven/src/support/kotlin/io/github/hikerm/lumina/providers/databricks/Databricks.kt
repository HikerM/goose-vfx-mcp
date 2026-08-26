package io.github.hikerm.lumina.providers.databricks

public fun provider(host: String, token: String): io.github.hikerm.lumina.Provider =
    io.github.hikerm.lumina.databricksProvider(host, token)

public fun defaultModel(): String = io.github.hikerm.lumina.databricksDefaultModel()
