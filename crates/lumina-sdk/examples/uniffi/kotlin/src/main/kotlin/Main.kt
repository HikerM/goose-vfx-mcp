import io.github.hikerm.lumina.MessageRole
import io.github.hikerm.lumina.ProviderMessage
import io.github.hikerm.lumina.ProviderModelConfig
import io.github.hikerm.lumina.streamFlow
import io.github.hikerm.lumina.providers.openai.defaultModel
import io.github.hikerm.lumina.providers.openai.provider as openAiProvider
import kotlinx.coroutines.runBlocking

fun main() = runBlocking {
    val apiKey = System.getenv("OPENAI_API_KEY")
    require(!apiKey.isNullOrBlank()) {
        "Set OPENAI_API_KEY before running this example."
    }

    val provider = openAiProvider(apiKey)
    val model = ProviderModelConfig(modelName = defaultModel())
    val messages = listOf(
        ProviderMessage(
            role = MessageRole.USER,
            text = "What is the capital of France? Answer in one sentence.",
        ),
    )

    provider
        .streamFlow(
            model,
            "You are a knowledgeable geography expert.",
            messages,
        )
        .collect { chunk ->
            chunk.text?.let { print(it) }
            chunk.usageJson?.let { println("\nusage: $it") }
        }
    println()
}
