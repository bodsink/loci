package notify

import notify.model.Message

interface Channel {
    fun deliver(message: Message)
}

class EmailChannel : Channel {
    override fun deliver(message: Message) {
        render(message)
    }

    private fun render(message: Message) {}
}

class NotificationService(private val channel: Channel) {
    fun notifyOrderPlaced(orderId: String) {
        val message = buildMessage(orderId)
        channel.deliver(message)
    }

    private fun buildMessage(orderId: String): Message {
        return Message()
    }
}

fun defaultService(): NotificationService {
    return NotificationService(EmailChannel())
}
