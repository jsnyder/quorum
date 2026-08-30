function processUser(user: any) {
    if (user.role === 'admin') {
        // Unsafe innerHTML
        document.getElementById('status')!.innerHTML = `<div>Welcome, ${user.name}</div>`;
    }
}

async function fetchData(url: string) {
    try {
        const response = await fetch(url);
        const data = await response.json();
        return data;
    } catch (e) {
        // Swallow error
    }
}

const config = {
    apiKey: "EY-987-654-321-ABC", // Generic API key
    debug: true
};
