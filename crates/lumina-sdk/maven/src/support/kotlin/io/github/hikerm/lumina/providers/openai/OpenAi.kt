package io.github.hikerm.lumina.providers.openai

public fun provider(apiKey: String): io.github.hikerm.lumina.Provider = io.github.hikerm.lumina.openaiProvider(apiKey)

public fun defaultModel(): String = io.github.hikerm.lumina.openaiDefaultModel()
