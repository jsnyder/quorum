#include "globals.h"
#include "defaults.h"
#include "string_utils.h"
#include "BleFingerprintCollection.h"
#include "mqtt.h"
#include <WiFi.h>
#include <memory>

bool pub(const char *topic, uint8_t qos, bool retain, const char *payload, size_t length, bool dup, uint16_t message_id)
{
    // Non-blocking publish - let AsyncMqttClient handle its own queue
    // Blocking delays here prevent MQTT keepalive responses
    uint16_t pid = mqttClient.publish(topic, qos, retain, payload, length, dup, message_id);
    if (pid == 0) {
        // AsyncMqttClient::publish has exactly two failure paths:
        //   _state != CONNECTED  ||  ESP.getMaxAllocHeap() < MQTT_MIN_FREE_MEMORY
        // so log both inputs. maxAlloc is the one that matters and the one that
        // is easy to miss: it is the largest *contiguous* block, not total free
        // heap. A node can sit on 88 KB free and still fail every publish once
        // the heap is fragmented below the 12 KB threshold, which reads as a
        // connectivity fault right up until you print this number.
        //
        // length is the caller's argument, not the wire length — AsyncMqttClient
        // substitutes strlen(payload) when it is 0, so print that instead to
        // avoid a misleading len=0 on every string publish.
        const unsigned wireLen = (payload != nullptr && length == 0) ? strlen(payload) : length;
        Log.printf("pub FAIL topic=%s qos=%u retain=%d len=%u connected=%d maxAlloc=%u (need %u) heap=%u minHeap=%u\r\n",
                   topic, (unsigned)qos, (int)retain, wireLen,
                   (int)mqttClient.connected(),
                   (unsigned)ESP.getMaxAllocHeap(), (unsigned)MQTT_MIN_FREE_MEMORY,
                   (unsigned)ESP.getFreeHeap(), (unsigned)ESP.getMinFreeHeap());
    }
    return pid;
}

bool pub(const char *topic, uint8_t qos, bool retain, JsonVariantConst jsonDoc, bool dup, uint16_t message_id)
{
    // Heap-allocate the serialized payload rather than using a VLA on the
    // caller's FreeRTOS task stack. AsyncMqttClient copies the payload
    // synchronously inside PublishOutPacket's ctor, so the buffer can be
    // freed as soon as publish() returns. See PR #2315 for why we moved off
    // the stack despite the nominal payload size being small.
    //
    // The (measureJson, serializeJson) pair is TOCTOU-racy against any
    // concurrent mutation of a shared JsonDocument. If measured and
    // serialized sizes disagree we refuse to publish rather than emit
    // truncated JSON that the broker would flag as malformed.
    size_t const jsonSize = measureJson(jsonDoc);
    std::unique_ptr<char[]> buffer(new (std::nothrow) char[jsonSize + 1]);
    if (!buffer) {
        log_w("pub: unable to allocate %u-byte JSON buffer on topic %s", (unsigned)(jsonSize + 1), topic);
        return false;
    }
    size_t const buffSize = serializeJson(jsonDoc, buffer.get(), jsonSize + 1);
    if (buffSize == 0 || buffSize != jsonSize) {
        log_w("pub: serialize mismatch on topic %s (measured=%u, serialized=%u)",
              topic, (unsigned)jsonSize, (unsigned)buffSize);
        return false;
    }
    return pub(topic, qos, retain, buffer.get(), buffSize, dup, message_id);
}

bool pub(const char *topic, uint8_t qos, bool retain, const JsonDocument &jsonDoc, bool dup, uint16_t message_id)
{
    // DynamicJsonDocument / BasicJsonDocument overflow is silent — writes
    // past pool capacity are dropped, producing valid JSON with missing
    // fields. Log once per publish so missing HA discovery entities or
    // truncated telemetry point at the underlying cause.
    if (jsonDoc.overflowed()) {
        log_w("pub: JSON doc overflowed (cap=%u, memUsage=%u) on topic %s — bump SHARED_JSON_DOC_CAPACITY",
              (unsigned)jsonDoc.capacity(), (unsigned)jsonDoc.memoryUsage(), topic);
    }
    return pub(topic, qos, retain, jsonDoc.as<JsonVariantConst>(), dup, message_id);
}

void commonDiscovery()
{
    doc.clear();
    auto identifiers = doc["dev"].createNestedArray("ids");
    identifiers.add(Sprintf("espresense_%06x", CHIPID));
    auto connections = doc["dev"].createNestedArray("cns");
    auto mac = connections.createNestedArray();
    mac.add("mac");
    mac.add(WiFi.macAddress());
    doc["dev"]["name"] = "ESPresense " + room;
    doc["dev"]["sa"] = room;
#ifdef VERSION
    doc["dev"]["sw"] = VERSION;
#endif
#ifdef FIRMWARE
    doc["dev"]["mf"] = "ESPresense (" FIRMWARE ")";
#endif
    doc["dev"]["cu"] = "http://" + localIp;
    doc["dev"]["mdl"] = String(ESP.getChipModel());
}

bool sendConnectivityDiscovery()
{
    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = "Connectivity";
    doc["uniq_id"] = Sprintf("espresense_%06x_connectivity", CHIPID);
    doc["json_attr_t"] = "~/telemetry";
    doc["stat_t"] = "~/status";
    doc["dev_cla"] = "connectivity";
    doc["pl_on"] = "online";
    doc["pl_off"] = "offline";

    const String discoveryTopic = Sprintf("%s/binary_sensor/espresense_%06x/connectivity/config", homeAssistantDiscoveryPrefix.c_str(), CHIPID);
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendTeleBinarySensorDiscovery(const String &name, const String &entityCategory, const String &temp, const String &devClass)
{
    auto slug = slugify(name);

    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = name;
    doc["uniq_id"] = Sprintf("espresense_%06x_%s", CHIPID, slug.c_str());
    doc["avty_t"] = "~/status";
    doc["stat_t"] = "~/telemetry";
    doc["value_template"] = temp;
    if (!entityCategory.isEmpty()) doc["entity_category"] = entityCategory;
    if (!devClass.isEmpty()) doc["dev_cla"] = devClass;

    const String discoveryTopic = Sprintf("%s/binary_sensor/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(), CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendTeleSensorDiscovery(const String &name, const String &entityCategory, const String &temp, const String &devClass, const String &units)
{
    auto slug = slugify(name);

    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = name;
    doc["uniq_id"] = Sprintf("espresense_%06x_%s", CHIPID, slug.c_str());
    doc["avty_t"] = "~/status";
    doc["stat_t"] = "~/telemetry";
    doc["value_template"] = temp;
    if (!entityCategory.isEmpty()) doc["entity_category"] = entityCategory;
    if (!units.isEmpty()) doc["unit_of_meas"] = units;
    if (!devClass.isEmpty()) doc["dev_cla"] = devClass;

    const String discoveryTopic = Sprintf("%s/sensor/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(),CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendSensorDiscovery(const String &name, const String &entityCategory, const String &devClass, const String &units, bool frcUpdate)
{
    auto slug = slugify(name);

    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = name;
    doc["uniq_id"] = Sprintf("espresense_%06x_%s", CHIPID, slug.c_str());
    doc["avty_t"] = "~/status";
    doc["stat_t"] = "~/" + slug;
    if (!entityCategory.isEmpty()) doc["entity_category"] = entityCategory;
    if (!units.isEmpty()) doc["unit_of_meas"] = units;
    if (!devClass.isEmpty()) doc["dev_cla"] = devClass;
    doc["frc_upd"] = frcUpdate;

    const String discoveryTopic = Sprintf("%s/sensor/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(), CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendBinarySensorDiscovery(const String &name, const String &entityCategory, const String &devClass)
{
    auto slug = slugify(name);

    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = name;
    doc["uniq_id"] = Sprintf("espresense_%06x_%s", CHIPID, slug.c_str());
    doc["avty_t"] = "~/status";
    doc["stat_t"] = "~/" + slug;
    if (!entityCategory.isEmpty()) doc["entity_category"] = entityCategory;
    if (!devClass.isEmpty()) doc["dev_cla"] = devClass;

    const String discoveryTopic = Sprintf("%s/binary_sensor/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(), CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendButtonDiscovery(const String &name, const String &entityCategory)
{
    auto slug = slugify(name);

    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = name;
    doc["uniq_id"] = Sprintf("espresense_%06x_%s", CHIPID, slug.c_str());
    doc["avty_t"] = "~/status";
    doc["stat_t"] = "~/" + slug;
    doc["cmd_t"] = "~/" + slug + "/set";
    if (!entityCategory.isEmpty()) doc["entity_category"] = entityCategory;

    const String discoveryTopic = Sprintf("%s/button/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(), CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendSwitchDiscovery(const String &name, const String &entityCategory)
{
    auto slug = slugify(name);

    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = name;
    doc["uniq_id"] = Sprintf("espresense_%06x_%s", CHIPID, slug.c_str());
    doc["avty_t"] = "~/status";
    doc["stat_t"] = "~/" + slug;
    doc["cmd_t"] = "~/" + slug + "/set";
    doc["entity_category"] = entityCategory;

    String const discoveryTopic = Sprintf("%s/switch/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(), CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendNumberDiscovery(const String &name, const String &entityCategory)
{
    auto slug = slugify(name);

    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = name;
    doc["uniq_id"] = Sprintf("espresense_%06x_%s", CHIPID, slug.c_str());
    doc["avty_t"] = "~/status";
    doc["stat_t"] = "~/" + slug;
    doc["cmd_t"] = "~/" + slug + "/set";
    doc["step"] = "0.1";
    if (!entityCategory.isEmpty()) doc["entity_category"] = entityCategory;

    const String discoveryTopic = Sprintf("%s/number/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(), CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendLightDiscovery(const String &name, const String &entityCategory, bool rgb, bool rgbw)
{
    auto slug = slugify(name);

    commonDiscovery();
    doc["~"] = roomsTopic;
    doc["name"] = name;
    doc["uniq_id"] = Sprintf("espresense_%06x_%s", CHIPID, slug.c_str());
    doc["schema"] = "json";
    doc["stat_t"] = "~/" + slug;
    doc["cmd_t"] = "~/" + slug + "/set";
    doc["brightness"] = true;

    if (rgbw) {
        doc["supported_color_modes"][0] = "rgbw";
    } else if (rgb) {
        doc["supported_color_modes"][0] = "rgb";
    } else {
        doc["supported_color_modes"][0] = "brightness";
    }

    if (!entityCategory.isEmpty()) doc["entity_category"] = entityCategory;

    const String discoveryTopic = Sprintf("%s/light/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(), CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, true, doc);
}

bool sendDeleteDiscovery(const String &domain, const String &name)
{
    auto slug = slugify(name);
    const String discoveryTopic = Sprintf("%s/%s/espresense_%06x/%s/config", homeAssistantDiscoveryPrefix.c_str(), domain.c_str(), CHIPID, slug.c_str());
    return pub(discoveryTopic.c_str(), 0, false, "");
}

/**
 * @brief Publish or update a device configuration to the channel settings topic.
 *
 * If an existing device configuration is found using the provided alias and its
 * stored id differs from the given id, that existing configuration is deleted
 * before publishing the new configuration. The published payload contains the
 * alias as the device identifier and the provided friendly name. When
 * calRssi is greater than NO_RSSI, an "rssi@1m" field is included.
 *
 * @param id Unique device id used to build the settings topic.
 * @param alias Device alias to include in the payload as the device identifier.
 * @param name Friendly name to include in the payload.
 * @param calRssi Calibration RSSI value; included as "rssi@1m" if greater than NO_RSSI.
 * @return true if the configuration publish succeeded, false otherwise.
 */
bool sendConfig(const String &id, const String &alias, const String &name, int calRssi)
{
    DeviceConfig existing;
    if (BleFingerprintCollection::FindDeviceConfigByAlias(alias, existing) && existing.id != id)
    {
        deleteConfig(existing.id);
    }
    Log.printf("%u Alias  | %s to %s\r\n", xPortGetCoreID(), id.c_str(), alias.c_str());
    DynamicJsonDocument json(256);
    json["id"] = alias;
    json["name"] = name;
    if (calRssi > NO_RSSI) json["rssi@1m"] = calRssi;

    const String settingsTopic = CHANNEL + String("/settings/") + id + "/config";
    return pub(settingsTopic.c_str(), 0, true, json);
}

/**
 * @brief Publish a deletion for a device configuration to the MQTT settings topic.
 *
 * Sends an empty retained payload to "CHANNEL/settings/{id}/config" to remove the stored configuration for the given device id.
 *
 * @param id Device identifier used to build the settings topic.
 * @return true if the MQTT publish succeeded, false otherwise.
 */
bool deleteConfig(const String &id)
{
    Log.printf("%u Delete | %s\r\n", xPortGetCoreID(), id.c_str());
    const String settingsTopic = CHANNEL + String("/settings/") + id + "/config";
    return pub(settingsTopic.c_str(), 0, true, "");
}