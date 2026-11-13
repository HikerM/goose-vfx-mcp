import assert from "node:assert/strict";
import { test } from "node:test";
import { LuminaExtClient } from "../src/generated/client.gen.ts";
import type { NativeTaskDescriptionDto } from "../src/generated/types.gen.ts";

function videoTask(
  profile: string,
  fps: number,
  hasAudio: boolean,
): NativeTaskDescriptionDto {
  return {
    identity: {
      installationId: profile,
      versionId: profile + "-v1",
      manifestDigest: "manifest",
      runtimeDigest: "runtime",
      componentBindingDigest: profile + "-components",
      taskVariant: profile + "/video",
      parameterSchemaId: profile + "-parameters",
    },
    intent: hasAudio ? "text_to_video_with_audio" : "text_to_video",
    descriptor: {
      family: "generation",
      parameters: {
        schemaVersion: 1,
        profile,
        fields: {
          fps: {
            valueType: "integer",
            support: "native_supported",
            default: fps,
            enumValues: [],
            constraints: [],
            minimum: fps,
            maximum: fps,
          },
          audioMode: {
            valueType: "enum",
            support: "native_supported",
            enumValues: [hasAudio ? "joint_audio_video" : "video_only"],
            constraints: [],
          },
        },
        executionStatus: "needs_resource_admission",
        evidence: profile + "-test-evidence",
        limitations: [],
      },
    },
  };
}

test("task discovery preserves each model's contract and sends its complete identity on lookup", async () => {
  const tasks = [videoTask("wan", 8, false), videoTask("h3", 24, true)];
  for (const [language, temperature] of [["french", 0.7], ["spanish", 0.5]] as const) {
    tasks.push({
      identity: {
        installationId: language,
        versionId: language + "-v1",
        manifestDigest: language + "-manifest",
        runtimeDigest: "audio-runtime",
        componentBindingDigest: language + "-weights",
        taskVariant: "speech",
        parameterSchemaId: language + "-options",
      },
      intent: "speech_synthesis",
      descriptor: {
        family: "pocket_tts",
        parameters: {
          modelLanguage: language,
          outputSampleRate: 24000,
          fields: {
            temperature: {
              valueType: "f32", required: false, default: temperature,
              nativeOption: "--temperature", constraints: [], source: "test model catalog",
            },
          },
          unsupported: { speed: "Unavailable in this adapter" },
          evidence: language + "-test-evidence",
          limitations: [],
        },
      },
    });
  }
  for (const task of tasks) {
    const client = new LuminaExtClient({
      async extMethod(method, params) {
        if (method === "_lumina/unstable/local-inference/tasks/list") {
          assert.deepEqual(params, { sessionId: "chat", intent: task.intent });
          return { tasks: [task] };
        }
        assert.equal(method, "_lumina/unstable/local-inference/tasks/describe");
        assert.deepEqual(params, {
          sessionId: "chat",
          identity: task.identity,
        });
        return { status: "found", task };
      },
    });
    const list = await client.localInferenceTasksList_unstable({
      sessionId: "chat",
      intent: task.intent,
    });
    assert.deepEqual(list.tasks, [task]);
    const selected = await client.localInferenceTasksDescribe_unstable({
      sessionId: "chat",
      identity: list.tasks[0].identity,
    });
    assert.deepEqual(selected, { status: "found", task });
  }
});
